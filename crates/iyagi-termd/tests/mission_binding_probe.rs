#![cfg(unix)]
use iyagi_termd_lib::{
    agent_runtime::{fake::fake_binding, local_probe},
    mission::{artifacts::ArtifactStore, MissionService},
};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use term_contracts::{ids::ConnectionId, mission::rpc::ProbeModel, mission::types::*};
use term_storage::Storage;

fn program(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
fn service(dir: &Path) -> Arc<MissionService> {
    with_models(dir, |_, _, _| Vec::new())
}
/// Model-picker hints are injected: a probe must never depend on spawning the
/// CLI a second time, and a listing must never change the probe's outcome.
///
/// The local self-check is left un-run (11 §7 DI) so these cases keep judging
/// exactly one thing — the installation observation. The cases that are about
/// the measurement use [`with_prober`].
fn with_models(
    dir: &Path,
    lister: impl Fn(RuntimeKind, &str, Option<&str>) -> Vec<ProbeModel> + Send + Sync + 'static,
) -> Arc<MissionService> {
    let storage = Arc::new(Storage::open(dir.join("state.db")).unwrap());
    Arc::new(
        MissionService::new(
            storage.clone(),
            ArtifactStore::new(storage, dir.join("artifacts")),
        )
        .with_model_lister(lister)
        .without_local_probe(),
    )
}

fn passing_report() -> LocalProbeReport {
    LocalProbeReport {
        protocol_ok: true,
        sandbox_cases_passed: Some(12),
        sandbox_cases_total: Some(12),
        model_listed: Some(true),
        failures: vec![],
    }
}

/// A service whose `binding.probe` measures whatever the shared cell holds, so
/// a case can change what this machine "is" between two probes without ever
/// spawning a CLI.
fn with_prober(dir: &Path, report: Arc<Mutex<LocalProbeReport>>) -> Arc<MissionService> {
    let storage = Arc::new(Storage::open(dir.join("state.db")).unwrap());
    Arc::new(
        MissionService::new(
            storage.clone(),
            ArtifactStore::new(storage, dir.join("artifacts")),
        )
        .with_model_lister(|_, _, _| Vec::new())
        .with_local_prober(move |binding, program, version, _env, budget| {
            assert!(!program.is_empty(), "only an observed CLI is measured");
            assert!(!version.is_empty(), "the observed version is measured");
            assert_eq!(binding.runtime, RuntimeKind::Codex);
            assert_eq!(budget, local_probe::BUDGET);
            report.lock().unwrap().clone()
        }),
    )
}

/// Write straight into the stored document, the way accumulated Run evidence
/// reaches it, without running a mission.
fn patch_stored(dir: &Path, patch: impl Fn(&mut Value)) -> Value {
    let storage = Storage::open(dir.join("state.db")).unwrap();
    let mut document = storage.mission_bindings().unwrap().remove(0);
    patch(&mut document);
    storage
        .save_mission_binding(
            Id::generate(),
            "run-evidence-fixture",
            &"c".repeat(64),
            document["revision"].as_str().unwrap().parse().unwrap(),
            document,
            "2026-09-19T00:00:00Z".into(),
        )
        .unwrap()
        .document
}
fn call(service: &MissionService, method: &str, params: Value) -> Value {
    service
        .handle(&ConnectionId::generate(), method, &params)
        .unwrap_or_else(|e| panic!("{method}: {e:?}"))
        .result
}
fn create(service: &MissionService, path: &Path) -> Value {
    let mut binding = fake_binding();
    binding.runtime = RuntimeKind::Codex;
    binding.provider_id = "openai".into();
    binding.auth_route = AuthRoute::Subscription;
    binding.credential_ref = None;
    binding.endpoint_ref = None;
    binding.program = path.to_string_lossy().into();
    binding.runtime_version = Some("stale version".into());
    binding.checked_at = None;
    binding.enabled = false;
    call(
        service,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":"0","binding":binding}),
    )["binding"]
        .clone()
}

#[test]
fn a_file_changed_during_probe_clears_the_previous_private_observation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli");
    program(&path, "printf 'codex-cli 0.154.0\\n'");
    let svc = service(dir.path());
    let saved = create(&svc, &path);
    let first = call(&svc, "binding.probe", json!({"binding_id":saved["id"]}));
    assert_eq!(first["installation"], "verified");
    program(
        &path,
        "printf '#!/bin/sh\\nprintf changed\\n' > \"$0\"; printf 'codex-cli 0.154.0\\n'",
    );
    let failed = call(&svc, "binding.probe", json!({"binding_id":saved["id"]}));
    assert_eq!(failed["installation"], "failed");
    assert!(failed["binding"]["runtime_version"].is_null());
    assert!(failed["binding"]["capabilities"]
        .as_object()
        .unwrap()
        .values()
        .all(|c| c["supported"] == false));
    drop(svc);
    let reopened = service(dir.path());
    assert_eq!(
        call(&reopened, "binding.list", json!({}))["bindings"][0],
        failed["binding"]
    );
}

#[test]
fn installation_result_persists_without_changing_user_enablement_or_retaining_stale_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli with spaces");
    program(&path, "printf 'codex-cli 0.154.0\\n'");
    let svc = service(dir.path());
    let saved = create(&svc, &path);
    let probe = call(&svc, "binding.probe", json!({"binding_id":saved["id"]}));
    assert_eq!(probe["installation"], "verified");
    assert_eq!(probe["models"], json!([]));
    assert_eq!(probe["binding"]["enabled"], false);
    assert_eq!(probe["binding"]["runtime_version"], "0.154.0");
    assert_ne!(probe["binding"]["revision"], saved["revision"]);
    assert!(probe["binding"]["checked_at"].is_string());
    for (body, status) in [
        ("printf 'codex-cli 9.99.9-beta.1\\n'", "verified"),
        ("printf 'codex-cli 0.154.0\\n'; exit 1", "failed"),
        ("printf 'not a CLI version'", "unrecognized_version"),
    ] {
        program(&path, body);
        let result = call(&svc, "binding.probe", json!({"binding_id":saved["id"]}));
        assert_eq!(result["installation"], status);
        assert_eq!(
            result["binding"]["runtime_version"],
            if status == "verified" {
                json!("9.99.9-beta.1")
            } else {
                Value::Null
            }
        );
        assert!(result["binding"]["capabilities"]
            .as_object()
            .unwrap()
            .values()
            .all(|c| c["supported"] == false));
    }
    std::fs::remove_file(&path).unwrap();
    let missing = call(&svc, "binding.probe", json!({"binding_id":saved["id"]}));
    assert_eq!(missing["installation"], "not_found");
    assert!(missing["binding"]["runtime_version"].is_null());
    drop(svc);
    let reopened = service(dir.path());
    assert_eq!(
        call(&reopened, "binding.list", json!({}))["bindings"][0],
        missing["binding"]
    );
}

#[test]
fn changing_connection_identity_invalidates_evidence_but_label_edits_keep_the_saved_probe() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli");
    program(&path, "printf 'codex-cli 0.154.0\\n'");
    let svc = service(dir.path());
    let binding = create(&svc, &path);
    for (key, value) in [
        ("model_id", json!("other-model")),
        ("provider_id", json!("other-provider")),
        ("auth_route", json!("api_key")),
        ("program", json!("/different/cli")),
    ] {
        let checked =
            call(&svc, "binding.probe", json!({"binding_id":binding["id"]}))["binding"].clone();
        let mut edited = checked.clone();
        edited[key] = value;
        let params = json!({"request_id":Id::generate(),"expected_revision":checked["revision"],"binding":edited});
        let changed = call(&svc, "binding.save", params.clone());
        assert_eq!(call(&svc, "binding.save", params), changed);
        assert!(changed["binding"]["runtime_version"].is_null());
        assert!(changed["binding"]["checked_at"].is_null());
        assert!(changed["binding"]["capabilities"]
            .as_object()
            .unwrap()
            .values()
            .all(|c| c["supported"] == false));
        let mut restore = binding.clone();
        restore["revision"] = changed["binding"]["revision"].clone();
        call(
            &svc,
            "binding.save",
            json!({"request_id":Id::generate(),"expected_revision":restore["revision"],"binding":restore}),
        );
    }
    let checked =
        call(&svc, "binding.probe", json!({"binding_id":binding["id"]}))["binding"].clone();
    let mut label = checked.clone();
    label["label"] = json!("New label");
    label["runtime_version"] = json!("client stale data");
    label["capabilities"]["scoped_write"]["supported"] = json!(true);
    let saved = call(
        &svc,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":checked["revision"],"binding":label}),
    );
    for key in ["runtime_version", "checked_at", "capabilities"] {
        assert_eq!(saved["binding"][key], checked[key]);
    }
}

#[test]
fn probe_cannot_overwrite_a_concurrent_settings_edit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli");
    program(
        &path,
        "touch \"$0.started\"; sleep 0.5; printf 'codex-cli 0.154.0\\n'",
    );
    let svc = service(dir.path());
    let saved = create(&svc, &path);
    let child_svc = svc.clone();
    let id = saved["id"].clone();
    let worker = std::thread::spawn(move || {
        child_svc.handle(
            &ConnectionId::generate(),
            "binding.probe",
            &json!({"binding_id":id}),
        )
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    while !path.with_extension("started").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(path.with_extension("started").exists());
    let mut edited = saved.clone();
    edited["model_id"] = json!("concurrently-selected-model");
    let changed = call(
        &svc,
        "binding.save",
        json!({"request_id":Id::generate(),"expected_revision":saved["revision"],"binding":edited}),
    );
    let error = worker
        .join()
        .unwrap()
        .err()
        .expect("stale probe must be rejected");
    assert_eq!(
        error.code,
        term_contracts::mission::MissionErrorCode::RevisionConflict
    );
    assert_eq!(
        call(&svc, "binding.list", json!({}))["bindings"][0],
        changed["binding"]
    );
}

#[test]
fn clients_cannot_create_or_restore_installation_evidence_even_by_copying_private_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli");
    program(&path, "printf 'codex-cli 0.154.0\\n'");
    let svc = service(dir.path());
    let binding = create(&svc, &path);
    assert!(binding["runtime_version"].is_null());
    assert!(binding["checked_at"].is_null());
    assert!(binding["capabilities"]
        .as_object()
        .unwrap()
        .values()
        .all(|c| c["supported"] == false));
    let checked =
        call(&svc, "binding.probe", json!({"binding_id":binding["id"]}))["binding"].clone();
    assert!(checked.get("_installation_observation").is_none());
    let storage = Storage::open(dir.path().join("state.db")).unwrap();
    let private = storage.mission_bindings().unwrap()[0]["_installation_observation"].clone();
    assert!(private.is_object());

    // A second ID, or the same ID after an identity change, cannot import it.
    for new_id in [true, false] {
        let mut forged = checked.clone();
        if new_id {
            forged["id"] = json!(Id::generate());
        }
        forged["revision"] = if new_id {
            json!("0")
        } else {
            checked["revision"].clone()
        };
        forged["program"] = json!("/different/program");
        forged["runtime_version"] = json!("0.154.0");
        forged["checked_at"] = json!("2026-09-16T00:00:00Z");
        forged["capabilities"] = serde_json::to_value(fake_binding().capabilities).unwrap();
        forged["_installation_observation"] = private.clone();
        let params = json!({"request_id":Id::generate(),"expected_revision":forged["revision"],"binding":forged});
        let saved = call(&svc, "binding.save", params.clone());
        assert_eq!(call(&svc, "binding.save", params), saved);
        assert!(saved["binding"]["runtime_version"].is_null());
        assert!(saved["binding"]["checked_at"].is_null());
        assert!(saved["binding"]["capabilities"]
            .as_object()
            .unwrap()
            .values()
            .all(|c| c["supported"] == false));
        assert!(saved["binding"].get("_installation_observation").is_none());
    }
}

#[test]
fn legacy_claims_and_observations_from_another_os_are_not_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli");
    program(&path, "printf 'codex-cli 0.154.0\\n'");
    let svc = service(dir.path());
    let saved = create(&svc, &path);
    call(&svc, "binding.probe", json!({"binding_id":saved["id"]}));
    let storage = Storage::open(dir.path().join("state.db")).unwrap();
    let mut document = storage.mission_bindings().unwrap().remove(0);
    for foreign in [true, false] {
        document["runtime_version"] = json!("0.154.0");
        document["checked_at"] = json!("2026-09-16T00:00:00Z");
        document["capabilities"] = serde_json::to_value(fake_binding().capabilities).unwrap();
        if foreign {
            document["_installation_observation"]["os"] = json!("different-os");
        } else {
            document
                .as_object_mut()
                .unwrap()
                .remove("_installation_observation");
        }
        document = storage
            .save_mission_binding(
                Id::generate(),
                "legacy-fixture",
                &"a".repeat(64),
                document["revision"].as_str().unwrap().parse().unwrap(),
                document,
                "2026-09-16T00:00:00Z".into(),
            )
            .unwrap()
            .document;
        let listed = call(&svc, "binding.list", json!({}))["bindings"][0].clone();
        assert!(listed["runtime_version"].is_null());
        assert!(listed["checked_at"].is_null());
        assert!(listed["capabilities"]
            .as_object()
            .unwrap()
            .values()
            .all(|c| c["supported"] == false));
    }
    drop(svc);
    let reopened = service(dir.path());
    assert!(call(&reopened, "binding.list", json!({}))["bindings"][0]["runtime_version"].is_null());
}

#[test]
fn stored_observation_recomputes_capabilities_and_survives_atomic_save_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli");
    program(&path, "printf 'codex-cli 0.154.0\\n'");
    let svc = service(dir.path());
    let saved = create(&svc, &path);
    let before = call(&svc, "binding.probe", json!({"binding_id":saved["id"]}))["binding"].clone();
    let storage = Storage::open(dir.path().join("state.db")).unwrap();
    let mut forged = storage.mission_bindings().unwrap().remove(0);
    forged["capabilities"] = serde_json::to_value(fake_binding().capabilities).unwrap();
    let forged = storage
        .save_mission_binding(
            Id::generate(),
            "legacy-fixture",
            &"b".repeat(64),
            forged["revision"].as_str().unwrap().parse().unwrap(),
            forged,
            "2026-09-16T00:00:00Z".into(),
        )
        .unwrap()
        .document;
    let listed = call(&svc, "binding.list", json!({}))["bindings"][0].clone();
    assert_eq!(listed["capabilities"], before["capabilities"]);
    assert_eq!(listed["runtime_version"], before["runtime_version"]);
    let db = rusqlite::Connection::open(dir.path().join("state.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_probe BEFORE UPDATE ON orch_bindings BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END;").unwrap();
    program(&path, "printf 'codex-cli 9.99.9\\n'");
    assert!(svc
        .handle(
            &ConnectionId::generate(),
            "binding.probe",
            &json!({"binding_id":saved["id"]})
        )
        .is_err());
    assert_eq!(
        storage.mission_bindings().unwrap()[0],
        forged,
        "failed CAS must not replace the private observation"
    );
    db.execute_batch("DROP TRIGGER reject_probe;").unwrap();
    let after = call(&svc, "binding.probe", json!({"binding_id":saved["id"]}));
    assert_eq!(after["binding"]["runtime_version"], "9.99.9");
}

#[test]
fn a_verified_probe_carries_the_runtimes_model_hints_and_a_failed_one_carries_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli");
    program(&path, "printf 'codex-cli 0.154.0\\n'");
    let svc = with_models(dir.path(), |runtime, program, provider| {
        // The probe knows the binding, so the listing can be narrowed to it.
        assert_eq!(runtime, RuntimeKind::Codex);
        assert_eq!(provider, Some("openai"));
        assert!(!program.is_empty());
        vec![ProbeModel {
            id: "gpt-listed".into(),
            efforts: vec!["high".into()],
        }]
    });
    let saved = create(&svc, &path);
    let probe = call(&svc, "binding.probe", json!({"binding_id":saved["id"]}));
    assert_eq!(probe["installation"], "verified");
    assert_eq!(
        probe["models"],
        json!([{"id":"gpt-listed","efforts":["high"]}])
    );
    // Hints are not evidence: the stored binding keeps its own model.
    assert_eq!(probe["binding"]["model_id"], saved["model_id"]);
    assert!(probe["binding"].get("models").is_none());

    // The CLI stops answering: an unverified installation asks for nothing.
    program(&path, "exit 1");
    let failed = call(
        &svc,
        "binding.probe",
        json!({"binding_id":probe["binding"]["id"]}),
    );
    assert_eq!(failed["installation"], "failed");
    assert_eq!(failed["models"], json!([]));
}

/// 11 §7: `binding.probe` is also the local self-check, and its report is the
/// only thing allowed to write `local_evidence`. The counters beside it belong
/// to one OS + version + model, so they survive a re-measurement of the same
/// installation and nothing else.
#[test]
fn a_probe_replaces_its_measurement_and_keeps_run_counters_for_the_same_installation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli");
    program(&path, "printf 'codex-cli 0.154.0\\n'");
    let report = Arc::new(Mutex::new(passing_report()));
    let svc = with_prober(dir.path(), report.clone());
    let saved = create(&svc, &path);
    assert!(
        saved["local_evidence"].is_null(),
        "a saved connection has measured nothing yet"
    );
    let first = call(&svc, "binding.probe", json!({"binding_id":saved["id"]}))["binding"].clone();
    assert_eq!(first["local_evidence"]["os"], std::env::consts::OS);
    assert_eq!(first["local_evidence"]["version"], "0.154.0");
    assert_eq!(first["local_evidence"]["model_id"], saved["model_id"]);
    assert_eq!(first["local_evidence"]["probe"], json!(passing_report()));
    assert!(first["local_evidence"]["probed_at"].is_string());
    assert_eq!(first["local_evidence"]["runs"]["succeeded_read_only"], 0);
    assert_eq!(
        first["local_evidence"]["runs"]["last_at"],
        Value::Null,
        "a probe is not a Run"
    );
    assert_eq!(
        first["capabilities"]["structured_result"]["supported"], true,
        "a passing protocol check is evidence on this machine (11 §4/§6)"
    );

    // Runs then accumulate against that exact installation.
    patch_stored(dir.path(), |document| {
        document["local_evidence"]["runs"]["succeeded_read_only"] = json!(3);
        document["local_evidence"]["runs"]["last_at"] = json!("2026-09-19T00:00:00Z");
    });
    // The same CLI is measured again and answers worse than before: the
    // report is replaced, the observed Runs are not re-judged by it.
    *report.lock().unwrap() = LocalProbeReport {
        protocol_ok: false,
        sandbox_cases_passed: Some(10),
        sandbox_cases_total: Some(12),
        model_listed: Some(false),
        failures: vec!["sandbox:writer_outside_workspace".into()],
    };
    let again = call(&svc, "binding.probe", json!({"binding_id":saved["id"]}))["binding"].clone();
    assert_eq!(again["local_evidence"]["probe"]["protocol_ok"], false);
    assert_eq!(
        again["local_evidence"]["probe"]["failures"],
        json!(["sandbox:writer_outside_workspace"])
    );
    assert_eq!(again["local_evidence"]["runs"]["succeeded_read_only"], 3);
    assert_eq!(
        again["local_evidence"]["runs"]["last_at"],
        "2026-09-19T00:00:00Z"
    );

    // Another model on the same CLI: the measurement is a property of the
    // executable, the counters are a property of the model.
    patch_stored(dir.path(), |document| {
        document["local_evidence"]["model_id"] = json!("some-other-model");
    });
    *report.lock().unwrap() = passing_report();
    let remodelled =
        call(&svc, "binding.probe", json!({"binding_id":saved["id"]}))["binding"].clone();
    assert_eq!(remodelled["local_evidence"]["model_id"], saved["model_id"]);
    assert_eq!(
        remodelled["local_evidence"]["runs"]["succeeded_read_only"],
        0
    );
    assert_eq!(remodelled["local_evidence"]["runs"]["last_at"], Value::Null);

    // The CLI is updated: nothing measured or observed for the old version
    // applies, and the user only had to press "check now" again.
    patch_stored(dir.path(), |document| {
        document["local_evidence"]["runs"]["succeeded_write"] = json!(5);
    });
    program(&path, "printf 'codex-cli 0.155.0\\n'");
    let updated = call(&svc, "binding.probe", json!({"binding_id":saved["id"]}))["binding"].clone();
    assert_eq!(updated["local_evidence"]["version"], "0.155.0");
    assert_eq!(updated["local_evidence"]["runs"]["succeeded_write"], 0);
    assert_eq!(updated["local_evidence"]["probe"], json!(passing_report()));

    // A CLI that stops answering leaves the stored measurement alone rather
    // than replacing it with a failure it never performed; the version it
    // belongs to is simply no longer observed, so nothing is claimed.
    program(&path, "exit 1");
    let failed = call(&svc, "binding.probe", json!({"binding_id":saved["id"]}));
    assert_eq!(failed["installation"], "failed");
    assert_eq!(
        failed["binding"]["local_evidence"]["version"], "0.155.0",
        "an unverified installation measures nothing"
    );
    assert!(failed["binding"]["capabilities"]
        .as_object()
        .unwrap()
        .values()
        .all(|c| c["supported"] == false));
}

/// 11 §2.4/§7: `local_evidence` is daemon-owned. A client may send anything;
/// what is stored is the daemon's own measurement, carried only onto the same
/// launch target.
#[test]
fn binding_save_discards_client_local_evidence_and_carries_the_daemons_own() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cli");
    program(&path, "printf 'codex-cli 0.154.0\\n'");
    let svc = with_prober(dir.path(), Arc::new(Mutex::new(passing_report())));
    let created = create(&svc, &path);
    call(&svc, "binding.probe", json!({"binding_id":created["id"]}));
    patch_stored(dir.path(), |document| {
        document["local_evidence"]["runs"]["succeeded_write"] = json!(4);
    });
    let measured = call(&svc, "binding.list", json!({}))["bindings"][0].clone();
    let save = |binding: &Value| -> Value {
        call(
            &svc,
            "binding.save",
            json!({"request_id":Id::generate(),"expected_revision":binding["revision"],"binding":binding}),
        )["binding"]
            .clone()
    };

    // A client that invents a measurement, or forges counters onto a real
    // one, changes nothing.
    let mut forged = measured.clone();
    forged["label"] = json!("Renamed");
    forged["local_evidence"]["probe"]["sandbox_cases_passed"] = json!(0);
    forged["local_evidence"]["runs"]["succeeded_write"] = json!(9999);
    forged["local_evidence"]["version"] = json!("99.0.0");
    let renamed = save(&forged);
    assert_eq!(renamed["label"], "Renamed");
    assert_eq!(renamed["local_evidence"], measured["local_evidence"]);

    // Only the model changed: the CLI was not measured again, so its report
    // still stands, but these counters were another model's.
    let mut remodelled = renamed.clone();
    remodelled["model_id"] = json!("another-model");
    let remodelled = save(&remodelled);
    assert_eq!(
        remodelled["local_evidence"]["probe"],
        measured["local_evidence"]["probe"]
    );
    assert_eq!(remodelled["local_evidence"]["model_id"], "another-model");
    assert_eq!(remodelled["local_evidence"]["runs"]["succeeded_write"], 0);

    // Another executable is another installation: nothing carries.
    let mut moved = remodelled.clone();
    moved["program"] = json!("/different/cli");
    let moved = save(&moved);
    assert!(moved["local_evidence"].is_null());
    assert!(
        moved["capabilities"]
            .as_object()
            .unwrap()
            .values()
            .all(|c| c["supported"] == false),
        "a connection with no observation and no measurement claims nothing"
    );
}

#[test]
#[ignore = "requires installed CLIs; executes only --version, without inference"]
fn installed_cli_version_smoke() {
    for (runtime, variable) in [
        (RuntimeKind::Codex, "IYAGI_INSTALL_CODEX"),
        (RuntimeKind::Claude, "IYAGI_INSTALL_CLAUDE"),
        (RuntimeKind::Opencode, "IYAGI_INSTALL_OPENCODE"),
    ] {
        let path = std::env::var(variable).expect("set an explicit CLI path");
        let observed = iyagi_termd_lib::agent_runtime::installation::observe(&path, runtime)
            .expect("installed CLI version and stable entrypoint");
        let identity = observed.executable.as_ref().unwrap();
        assert!(identity.is_valid());
        let repeated = iyagi_termd_lib::agent_runtime::installation::observe_expected(
            &path,
            runtime,
            Some(identity),
        )
        .unwrap();
        assert_eq!(observed, repeated);
        println!(
            "{runtime:?}: {} ({} bytes, sha256 {})",
            observed.version, identity.bytes, identity.sha256
        );
    }
}
