use super::*;
use iyagi_termd_lib::agent_runtime::{
    capability_evidence, installation::ProbeFailure, CancelReceipt, DeliveryReceipt, EventStream,
    RunProbe,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Counted {
    inner: Arc<dyn AgentAdapter>,
    starts: Arc<AtomicUsize>,
}
impl AgentAdapter for Counted {
    fn name(&self) -> &'static str {
        "counted-fixture"
    }
    fn start(&self, run: RunStart) -> std::io::Result<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.inner.start(run)
    }
    fn send_message(&self, id: &Id, body: &str) -> DeliveryReceipt {
        self.inner.send_message(id, body)
    }
    fn answer(&self, id: &Id, request: &str, answer: &str) -> DeliveryReceipt {
        self.inner.answer(id, request, answer)
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

fn configure(
    rig: &mut Rig,
    version: Arc<Mutex<Result<String, ProbeFailure>>>,
    support: Arc<AtomicBool>,
    write: bool,
) -> Id {
    rig.service = Arc::new(
        MissionService::new(
            rig.storage.clone(),
            ArtifactStore::new(rig.storage.clone(), rig.dir.path().join("missions")),
        )
        .with_binding_evidence(
            move |_, _| version.lock().unwrap().clone(),
            move |_, _, version| {
                if support.load(Ordering::SeqCst) && version == Some("fixture-v1") {
                    let mut caps = fake_binding().capabilities;
                    caps.scoped_write.supported = write;
                    caps
                } else {
                    capability_evidence::unclaimed()
                }
            },
        ),
    );
    let mut binding: Binding =
        serde_json::from_value(rig.storage.mission_bindings().unwrap().remove(0)).unwrap();
    binding.runtime = RuntimeKind::Codex;
    binding.provider_id = "openai".into();
    binding.auth_route = AuthRoute::Subscription;
    let id = binding.id.clone();
    rpc(
        &rig.service,
        &rig.conn,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":binding.revision,"binding":binding}),
    );
    id
}
fn counted(starts: Arc<AtomicUsize>) -> AdapterFactory {
    Arc::new(move |_| {
        Ok(Arc::new(Counted {
            starts: starts.clone(),
            inner: scripted(FakeScript {
                steps: vec![
                    FakeStep::Started {
                        session_id: None,
                        turn_id: None,
                    },
                    FakeStep::Delay { ms: 500 },
                ],
                ..Default::default()
            }),
        }))
    })
}

#[test]
fn unsupported_work_waits_without_attempts_and_resumes_after_a_verified_probe() {
    let mut rig = Rig::new(true, &["status", "--porcelain"]);
    let support = Arc::new(AtomicBool::new(false));
    let id = configure(
        &mut rig,
        Arc::new(Mutex::new(Ok("fixture-v1".into()))),
        support.clone(),
        true,
    );
    let starts = Arc::new(AtomicUsize::new(0));
    let mut actor = rig.actor(counted(starts.clone()));
    rig.tick_until(&mut actor, |s| {
        s.tasks[0].blocked_code.as_deref() == Some("capability_structured_result")
    });
    let before = rig.snapshot();
    assert!(before.runs.is_empty());
    assert_eq!(before.tasks[0].attempt_count, 0);
    assert_eq!(before.mission.automatic_start_count, 0);
    for _ in 0..3 {
        actor.tick().unwrap();
    }
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    support.store(true, Ordering::SeqCst);
    rpc(
        &rig.service,
        &rig.conn,
        "binding.probe",
        json!({"binding_id":id}),
    );
    rig.tick_until(&mut actor, |_| starts.load(Ordering::SeqCst) == 1);
    assert_eq!(rig.snapshot().runs.len(), 1);
    actor.shutdown();
}

#[test]
fn changed_or_unavailable_cli_fails_before_provider_start_without_automatic_retry() {
    for next in [
        Ok("fixture-v2".into()),
        Err(ProbeFailure::TimedOut),
        Err(ProbeFailure::NotFound),
    ] {
        let mut rig = Rig::new(true, &["status", "--porcelain"]);
        let version = Arc::new(Mutex::new(Ok("fixture-v1".into())));
        let id = configure(
            &mut rig,
            version.clone(),
            Arc::new(AtomicBool::new(true)),
            true,
        );
        rpc(
            &rig.service,
            &rig.conn,
            "binding.probe",
            json!({"binding_id":id}),
        );
        *version.lock().unwrap() = next;
        let starts = Arc::new(AtomicUsize::new(0));
        let mut actor = rig.actor(counted(starts.clone()));
        rig.tick_until(&mut actor, |s| {
            s.runs.iter().any(|r| r.state == RunState::Failed)
        });
        for _ in 0..3 {
            actor.tick().unwrap();
        }
        let s = rig.snapshot();
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        assert_eq!(s.runs.len(), 1);
        let run = &s.runs[0];
        assert_eq!(
            run.failure_code,
            Some(term_contracts::mission::MissionErrorCode::CapabilityUnsupported)
        );
        assert!(run.exec_id.is_none());
        assert!(run.provider_session_id.is_none());
        assert_eq!(
            run.binding_snapshot
                .as_ref()
                .unwrap()
                .runtime_version
                .as_deref(),
            Some("fixture-v1")
        );
        assert!(
            run.retry_evidence.is_some(),
            "local preflight proves that no task was submitted"
        );
        actor.shutdown();
    }
}

#[test]
fn read_only_evidence_cannot_apply_a_writer_plan() {
    let mut rig = Rig::new(true, &["status", "--porcelain"]);
    let id = configure(
        &mut rig,
        Arc::new(Mutex::new(Ok("fixture-v1".into()))),
        Arc::new(AtomicBool::new(true)),
        false,
    );
    rpc(
        &rig.service,
        &rig.conn,
        "binding.probe",
        json!({"binding_id":id}),
    );
    let seen = Arc::new(Mutex::new(vec![]));
    let mut actor = rig.actor(factory(seen.clone()));
    rig.tick_until(&mut actor, |s| {
        s.runs.iter().any(|r| r.state == RunState::Failed)
    });
    let s = rig.snapshot();
    assert_eq!(s.mission.plan_revision, 0);
    assert!(s.tasks.iter().all(|t| t.kind == TaskKind::Plan));
    assert_eq!(
        s.runs[0].failure_code,
        Some(term_contracts::mission::MissionErrorCode::CapabilityUnsupported)
    );
    assert!(seen.lock().unwrap().iter().all(|(kind, _)| kind == "plan"));
    actor.shutdown();
}
