//! Rust <-> TS contract parity and serde round-trip fixtures (ticket I02).
//!
//! `ts-rs` regenerates `src/generated/*.ts` during `cargo test`; the CI gate
//! afterwards requires `git diff --exit-code src/generated`.

use term_contracts::gate::{DaemonToHelper, GateTarget, HelperHello, HelperToDaemon};
use term_contracts::ids::{RequestId, U64String, WorkloadId};
use term_contracts::launch::{
    launch_fingerprint, ClaudeProvider, Enforcement, LaunchMode, LaunchPolicy, LaunchRequest,
    LaunchValidation, Priority,
};
use term_contracts::metrics::{HostSample, Metric, MetricQuality, PressureLevel};
use term_contracts::remote::{ExecutorChoice, GatewayEnvelope, RemoteHostConfig, RemoteHostStatus};
use term_contracts::rpc::{methods, Frame, RpcRequest};
use term_contracts::session::{SessionOutput, TerminalFrameKind};
use term_contracts::snapshot::{
    Capabilities, QueueEntry, QueueReason, ReliefPolicy, ReliefState, Snapshot, WorkloadSummary,
};
use term_contracts::state::{TerminalConnection, WorkloadState};
use term_contracts::workload::{GroupKind, ProcessOwnership, WorkloadRecord};

/// Snapshot entities are adjacently tagged (`{kind, value}`) by a hand-written
/// `Serialize`, and clients read them by that tag. Nothing else pins the shape:
/// a derive added later would silently switch the wire to serde's external
/// tagging, every entity of every snapshot would stop matching, and the mission
/// view would read as empty while the daemon worked normally. The TS export
/// carries the same tags (`#[ts(tag, content)]`), so this also guards the pair.
#[test]
fn snapshot_entities_are_adjacently_tagged_on_the_wire() {
    use term_contracts::mission::types::{Entity, Id, Workspace, WorkspaceKind, WorkspaceState};
    let workspace = Workspace {
        id: Id::generate(),
        mission_id: Id::generate(),
        path: "/tmp/workspace".into(),
        kind: WorkspaceKind::Worker,
        base_oid: "651093cb813ae32fbf1aeb3cf60d6af9ae0c7f12".into(),
        head_oid: "651093cb813ae32fbf1aeb3cf60d6af9ae0c7f12".into(),
        writer_run_id: None,
        lease_token: U64String::new(1).expect("lease token"),
        state: WorkspaceState::Ready,
        owned_by_daemon: true,
    };
    let wire = serde_json::to_value(Entity::Workspace(Box::new(workspace.clone()))).unwrap();
    assert_eq!(
        wire.as_object()
            .map(|map| map.keys().cloned().collect::<Vec<_>>()),
        Some(vec!["kind".to_string(), "value".to_string()])
    );
    assert_eq!(wire["kind"], "workspace");
    assert_eq!(wire["value"], serde_json::to_value(&workspace).unwrap());
    assert_eq!(
        serde_json::from_value::<Entity>(wire).unwrap(),
        Entity::Workspace(Box::new(workspace))
    );
}

#[test]
fn acceptance_without_reconciled_acknowledgements_preserves_legacy_wire_bytes() {
    use term_contracts::mission::{rpc::MissionAcceptParams, types::Id};
    let legacy = serde_json::json!({"request_id": Id::generate(), "mission_id": Id::generate(),
        "expected_revision": "5", "candidate_id": Id::generate(), "acknowledged_verification_ids": [], "human_requirement_ids": []});
    let absent: MissionAcceptParams = serde_json::from_value(legacy.clone()).unwrap();
    let mut explicit_null = legacy.clone();
    explicit_null["acknowledged_reconciled_run_ids"] = serde_json::Value::Null;
    let null: MissionAcceptParams = serde_json::from_value(explicit_null).unwrap();
    assert_eq!(absent, null);
    assert_eq!(
        serde_json::to_vec(&serde_json::to_value(absent).unwrap()).unwrap(),
        serde_json::to_vec(&legacy).unwrap()
    );
    let mut reviewed = legacy;
    reviewed["acknowledged_reconciled_run_ids"] = serde_json::json!([Id::generate()]);
    let parsed: MissionAcceptParams = serde_json::from_value(reviewed.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), reviewed);
}

fn sample_launch() -> LaunchRequest {
    let root = if cfg!(windows) {
        "C:/home/dev/repo"
    } else {
        "/home/dev/repo"
    };
    let mut env = std::collections::BTreeMap::new();
    env.insert("RUST_LOG".to_string(), "info".to_string());
    LaunchRequest {
        request_id: RequestId::generate(),
        profile_id: "11111111-2222-4333-8444-555555555555".to_string(),
        cwd: root.to_string(),
        program: format!("{root}/bin/codex"),
        argv: vec!["--profile".to_string(), "default".to_string()],
        env_overrides: env,
        mode: LaunchMode::Managed,
        executor: ExecutorChoice::Local,
        cols: 120,
        rows: 40,
        priority: Priority(1),
        policy: LaunchPolicy {
            reservation_bytes: U64String::new(2_147_483_648).unwrap(),
            cpu_slots: 1,
            enforcement: Enforcement::Prefer,
            memory_max_bytes: Some(U64String::new(4_294_967_296).unwrap()),
            cpu_max_cores: Some(2.0),
            pids_max: Some(128),
        },
        claude_provider: None,
    }
}

#[test]
fn launch_request_round_trips_through_json() {
    let req = sample_launch();
    let json = serde_json::to_string(&req).unwrap();
    let back: LaunchRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, req);
    assert_eq!(req.validate(), LaunchValidation::Valid);
    assert_eq!(launch_fingerprint(&back), launch_fingerprint(&req));
    assert_eq!(launch_fingerprint(&req).len(), 64);
}

/// The TS mirror (`src/generated/ClaudeProvider.ts`) is a `kind`-tagged
/// union; the wire shape of a routed launch is pinned here so the app and
/// the daemon cannot drift on the tag spelling.
#[test]
fn routed_launch_request_wire_shape() {
    let mut req = sample_launch();
    req.claude_provider = Some(ClaudeProvider::ZaiCodingPlan {
        main_model: "glm-5.3-flash[1m]".into(),
    });
    assert_eq!(req.validate(), LaunchValidation::Valid);
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(
        json["claude_provider"],
        serde_json::json!({"kind": "zai_coding_plan", "main_model": "glm-5.3-flash[1m]"})
    );
    let back: LaunchRequest = serde_json::from_value(json).unwrap();
    assert_eq!(back, req);
    assert_ne!(
        launch_fingerprint(&req),
        launch_fingerprint(&sample_launch())
    );
}

#[test]
fn wire_envelopes_round_trip() {
    let req = RpcRequest::new(
        "id-1",
        methods::WORKLOAD_LAUNCH,
        serde_json::to_value(sample_launch()).unwrap(),
    );
    let frame = Frame::from_json(serde_json::to_value(&req).unwrap()).unwrap();
    assert_eq!(frame, Frame::Request(req));

    let out = SessionOutput {
        session_id: term_contracts::ids::SessionId::generate(),
        epoch: "epoch-uuid".into(),
        seq: U64String::new(9).unwrap(),
        kind: TerminalFrameKind::Output,
        data_b64: "QUJD".into(),
        raw_len: 3,
        cols: None,
        rows: None,
    };
    let ev = term_contracts::rpc::RpcEvent {
        v: 1,
        event: term_contracts::rpc::RpcEventKind::SessionOutput,
        payload: serde_json::to_value(&out).unwrap(),
    };
    let frame = Frame::from_json(serde_json::to_value(&ev).unwrap()).unwrap();
    assert_eq!(frame, Frame::Event(ev));
}

#[test]
fn snapshot_round_trips() {
    let snap = Snapshot {
        revision: 42,
        host: HostSample {
            monotonic_ms: 12_345,
            physical_total_bytes: Metric::measured(
                "sysinfo",
                U64String::new(17_179_869_184).unwrap(),
            ),
            physical_available_bytes: Metric::measured(
                "sysinfo",
                U64String::new(10_737_418_240).unwrap(),
            ),
            physical_used_bytes: Some(Metric::measured(
                "sysinfo",
                U64String::new(5_368_709_120).unwrap(),
            )),
            swap_used_bytes: Metric::unavailable("sysinfo", "not exposed"),
            pressure: PressureLevel::Normal,
            cpu_pressure: PressureLevel::Warning,
            cpu_cores_used: Metric::estimated("sysinfo", 2.4),
            logical_cpu_count: 12,
            disks: vec![],
            interfaces: vec![],
        },
        workloads: vec![WorkloadSummary {
            workload_id: WorkloadId::generate(),
            session_id: Some(term_contracts::ids::SessionId::generate()),
            mode: LaunchMode::Managed,
            state: WorkloadState::Running,
            priority: Priority(0),
            title: "codex /repo".into(),
            cwd: "/repo".into(),
            program: "/usr/local/bin/codex".into(),
            reservation_bytes: U64String::new(2_147_483_648).unwrap(),
            cpu_slots: 1,
            enforcement: Enforcement::Observe,
            root_exited: false,
            cancel_requested: false,
            exit_code: None,
            last_error_code: None,
            queue_reason: None,
            connection: TerminalConnection::Attached,
            usage: None,
            agent: None,
            relief: ReliefState::Yielded {
                since_ms: U64String::new(1_000).unwrap(),
                manual: false,
                partial: true,
            },
            protected: true,
            guard: Default::default(),
            guard_warning: None,
        }],
        queue: vec![QueueEntry {
            workload_id: WorkloadId::generate(),
            request_id: RequestId::generate(),
            priority: Priority(2),
            effective_priority: Priority(1),
            queued_at_ms: 9_000,
            wait_reason: Some(QueueReason::WaitConcurrency),
        }],
        capabilities: Capabilities::observe_only("macos"),
        reconciliation_required: false,
        focused_session_ids: vec![term_contracts::ids::SessionId::generate()],
        relief_policy: ReliefPolicy { auto_yield: false },
        guard_policy: Default::default(),
    };
    let json = serde_json::to_string(&snap).unwrap();
    let back: Snapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back, snap);
    assert_eq!(back.host.available_bytes(), Some(10_737_418_240));
    assert_eq!(back.host.cpu_pressure, PressureLevel::Warning);
    assert_eq!(back.focused_session_ids.len(), 1);
    // 08 §2/§7: 완화 상태·보호 표시·정책이 태그 유니온 그대로 왕복한다.
    assert!(back.workloads[0].relief.is_yielded());
    assert!(back.workloads[0].protected);
    assert!(!back.relief_policy.auto_yield);
    assert_eq!(
        back.host.swap_used_bytes.quality,
        MetricQuality::Unavailable
    );
}

#[test]
fn workload_record_and_ownership_round_trip() {
    let rec = WorkloadRecord {
        id: WorkloadId::generate(),
        mode: LaunchMode::Shell,
        state: WorkloadState::Draining,
        priority: Priority(2),
        reservation_bytes: U64String::new(1).unwrap(),
        cpu_slots: 1,
        enforcement: Enforcement::Observe,
        memory_max_bytes: None,
        cpu_max_cores: None,
        pids_max: None,
        queue_reason: None,
        cancel_requested: true,
        root_exited: true,
        exit_code: None,
        last_error_code: None,
    };
    let back: WorkloadRecord = serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();
    assert_eq!(back, rec);

    let own = ProcessOwnership {
        workload_id: WorkloadId::generate(),
        identity: term_contracts::ids::ProcessIdentity {
            pid: 4242,
            start_token: "777".into(),
            boot_id: "boot-1".into(),
        },
        group_kind: GroupKind::Job,
        group_reference: Some("job-obj-handle".into()),
        coverage: term_contracts::metrics::UsageCoverage::Group,
    };
    let back: ProcessOwnership =
        serde_json::from_str(&serde_json::to_string(&own).unwrap()).unwrap();
    assert_eq!(back, own);
}

#[test]
fn gate_protocol_round_trips() {
    let hello = HelperHello {
        nonce: "ab".repeat(128),
        identity: term_contracts::ids::ProcessIdentity {
            pid: 9,
            start_token: "1".into(),
            boot_id: "b".into(),
        },
    };
    let back: HelperHello = serde_json::from_str(&serde_json::to_string(&hello).unwrap()).unwrap();
    assert_eq!(back, hello);

    let target = GateTarget {
        program: "/bin/zsh".into(),
        argv: vec!["-l".into()],
        env_overrides: Default::default(),
        env_clear: false,
        env_remove: Vec::new(),
        cwd: "/home/dev".into(),
    };
    let back: GateTarget = serde_json::from_str(&serde_json::to_string(&target).unwrap()).unwrap();
    assert_eq!(back, target);

    assert_eq!(
        serde_json::from_str::<DaemonToHelper>(
            &serde_json::to_string(&DaemonToHelper::Abort).unwrap()
        )
        .unwrap(),
        DaemonToHelper::Abort
    );
    assert_eq!(
        serde_json::from_str::<HelperToDaemon>(
            &serde_json::to_string(&HelperToDaemon::Started).unwrap()
        )
        .unwrap(),
        HelperToDaemon::Started
    );
}

#[test]
fn remote_contracts_round_trip_and_backcompat() {
    // Old launch JSON (no executor field) parses as Local.
    let req = sample_launch();
    let mut json = serde_json::to_value(&req).unwrap();
    json.as_object_mut().unwrap().remove("executor");
    let back: LaunchRequest = serde_json::from_value(json).unwrap();
    assert_eq!(back.executor, ExecutorChoice::Local);

    let host = RemoteHostConfig {
        id: "host-1".into(),
        ssh_config_alias: "buildbox".into(),
        label: "빌드 서버".into(),
        runner_path: "/usr/local/bin/iyagi-termd".into(),
        protocol_version: 1,
    };
    assert!(host.validate().is_ok());
    let back: RemoteHostConfig =
        serde_json::from_str(&serde_json::to_string(&host).unwrap()).unwrap();
    assert_eq!(back, host);

    let status = RemoteHostStatus {
        host_id: "host-1".into(),
        state: term_contracts::remote::RemoteConnectionState::Reconnecting,
        daemon_id: Some("d".into()),
        remote_capabilities: None,
        round_trip_ms: Some(12),
        reconnect_attempt: 3,
        last_error: None,
    };
    let back: RemoteHostStatus =
        serde_json::from_str(&serde_json::to_string(&status).unwrap()).unwrap();
    assert_eq!(back, status);

    let env = GatewayEnvelope::wrap(GatewayEnvelope::CONTROL, serde_json::json!({"v": 1}));
    let back: GatewayEnvelope =
        serde_json::from_str(&serde_json::to_string(&env).unwrap()).unwrap();
    assert_eq!(back, env);
}
