#![cfg(unix)]
//! `runtime.detect`: read-only CLI discovery through the mission service.
use iyagi_termd_lib::{
    agent_runtime::{
        capability_evidence,
        detection::{self, DetectionEnv},
        fake::fake_binding,
        installation, local_probe,
    },
    mission::{artifacts::ArtifactStore, service::is_mission_method, MissionService},
};
use serde_json::{json, Value};
use std::{path::Path, sync::Arc};
use term_contracts::{
    ids::ConnectionId,
    mission::{
        rpc::{ProbeModel, RuntimeAltModels, RuntimeDetectResult},
        types::*,
    },
};
use term_storage::Storage;

fn program(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Picker hints are injected: no suite may spawn a CLI to list models, and
/// the stub proves the listing reaches the wire without becoming evidence.
///
/// A breach shows up in the listed models rather than as a panic here, because
/// detection deliberately turns a panicking listing into an empty list for that
/// row — `a_panicking_model_listing_empties_only_its_own_row` holds it to
/// that. So each runtime's candidates are asserted by the caller instead: a
/// wrong provider empties the row, and an OpenCode row that was asked at all
/// carries a marker id.
fn stub_models(runtime: RuntimeKind, program: &str, provider: Option<&str>) -> Vec<ProbeModel> {
    assert!(!program.is_empty(), "only a found CLI is asked");
    match runtime {
        // Detection asks with the provider its own one-click setup would save,
        // so a row cannot offer a model the binding it creates could not run.
        RuntimeKind::Codex => {
            assert_eq!(provider, Some("openai"), "the row's suggested provider");
            vec![
                ProbeModel {
                    id: "gpt-listed".into(),
                    efforts: vec!["high".into()],
                },
                ProbeModel {
                    id: "gpt-configured".into(),
                    efforts: vec![],
                },
            ]
        }
        RuntimeKind::Claude => {
            assert_eq!(provider, Some("anthropic"), "the row's suggested provider");
            vec![ProbeModel {
                id: "opus".into(),
                efforts: vec![],
            }]
        }
        // A detection row cannot become an OpenCode connection: it has no
        // credential/endpoint reference to give one, so its roles stay empty
        // and `binding.probe` judges it after the connection is saved. Asking
        // buys an unusable hint for the CLI's start-up cost, so it must not
        // happen — and this marker makes it visible on the wire if it does.
        RuntimeKind::Opencode => {
            vec![ProbeModel {
                id: "opencode-must-not-be-asked".into(),
                efforts: vec![],
            }]
        }
        RuntimeKind::Fake => Vec::new(),
    }
}

/// A listing that panics for exactly one runtime, so the rows can be told
/// apart: `attach_model_hints` spawns one thread per row and reaps them with
/// `join().unwrap_or_default()`, which is the only reason a panicking probe is
/// not the whole call's failure.
///
/// Claude answers normally on purpose — an isolated failure has to be visible
/// as one empty row next to a full one, not as an empty result everywhere.
fn panic_for_codex(runtime: RuntimeKind, program: &str, provider: Option<&str>) -> Vec<ProbeModel> {
    assert!(!program.is_empty(), "only a found CLI is asked");
    match runtime {
        RuntimeKind::Codex => panic!("the fixture listing for {provider:?} panics"),
        RuntimeKind::Claude => vec![ProbeModel {
            id: "opus".into(),
            efforts: vec!["high".into()],
        }],
        RuntimeKind::Opencode | RuntimeKind::Fake => Vec::new(),
    }
}

fn call(service: &MissionService, method: &str, params: Value) -> Value {
    service
        .handle(&ConnectionId::generate(), method, &params)
        .unwrap_or_else(|e| panic!("{method}: {e:?}"))
        .result
}

/// Does the *shipped* registry claim anything for the connection this row's
/// one-click setup would save (11 §7 rule 2)? Whether it does depends on the
/// OS and architecture this suite runs on — a macOS x86_64 machine has no
/// recorded Codex fixture at all — so the expected grade is derived the same
/// way the daemon derives it instead of pinning one platform's answer.
fn shipped_claims(runtime: RuntimeKind, model_id: &str, version: &str) -> bool {
    let binding = detection::candidate_binding(
        runtime,
        "/fixture/cli",
        capability_evidence::evidence_provider_id(runtime).unwrap_or_default(),
        model_id,
        version,
    );
    capability_evidence::claims_any(&capability_evidence::evidence_for_binding(
        &binding,
        std::env::consts::OS,
        Some(version),
    ))
}

/// A report a `runtime.detect` row would get from this machine.
fn cheap_report(protocol_ok: bool) -> LocalProbeReport {
    LocalProbeReport {
        protocol_ok,
        sandbox_cases_passed: None,
        sandbox_cases_total: None,
        model_listed: Some(true),
        failures: if protocol_ok {
            vec![]
        } else {
            vec![local_probe::FAIL_HELP_UNAVAILABLE.into()]
        },
    }
}

#[test]
fn detection_reports_installed_clis_and_start_gate_roles_without_storing_anything() {
    assert!(is_mission_method("runtime.detect"));
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let home = dir.path().join("home");
    program(&bin.join("codex"), "printf 'codex-cli 0.154.0\\n'");
    program(&bin.join("claude"), "printf '2.1.270 (Claude Code)\\n'");
    write(&home.join(".codex").join("auth.json"), "{}");
    write(
        &home.join(".codex").join("config.toml"),
        "model = \"gpt-configured\"\n",
    );
    write(
        &home.join(".claude.json"),
        "{\"oauthAccount\":{\"accountUuid\":\"fixture\"}}",
    );
    write(
        &home.join(".claude").join("settings.json"),
        "{\"model\":\"opus\"}",
    );

    // The binding the setup would save uses the OS-pinned model when live
    // evidence exists for this version/architecture, otherwise the user's
    // configured default.
    let os = std::env::consts::OS;
    let codex_model = capability_evidence::evidence_model_id(RuntimeKind::Codex, os)
        .filter(|_| {
            capability_evidence::capabilities_for(RuntimeKind::Codex, os, Some("0.154.0"))
                .structured_result
                .supported
        })
        .unwrap_or("gpt-configured");
    let storage = Arc::new(Storage::open(dir.path().join("state.db")).unwrap());
    let env = {
        let (bin, home) = (bin.clone(), home.clone());
        move || DetectionEnv {
            path: Some(bin.clone().into_os_string()),
            home: Some(home.clone()),
            ..DetectionEnv::default()
        }
    };
    let service = MissionService::new(
        storage.clone(),
        ArtifactStore::new(storage, dir.path().join("artifacts")),
    )
    .with_binding_evidence(
        |program, runtime| installation::observe(program, runtime).map(|found| found.version),
        move |binding, _, version| {
            let managed = binding.auth_route == AuthRoute::Subscription
                && binding.credential_ref.is_none()
                && binding.endpoint_ref.is_none()
                && binding.enabled;
            let mut caps = fake_binding().capabilities;
            match (binding.runtime, binding.provider_id.as_str(), version) {
                (RuntimeKind::Codex, "openai", Some("0.154.0"))
                    if managed && binding.model_id == codex_model =>
                {
                    caps
                }
                // Read-only evidence only: write roles must be excluded.
                (RuntimeKind::Claude, "anthropic", Some("2.1.270")) if managed => {
                    caps.scoped_write = Support {
                        supported: false,
                        reason_code: Some("fixture_read_only".into()),
                    };
                    caps
                }
                _ => capability_evidence::unclaimed(),
            }
        },
    )
    .with_detection_env(env)
    .with_model_lister(stub_models);

    let result = call(&service, "runtime.detect", json!({}));
    let typed: RuntimeDetectResult = serde_json::from_value(result.clone()).unwrap();
    assert_eq!(typed.runtimes.len(), 3);
    let proven = if codex_model == "gpt-configured" {
        Value::Null
    } else {
        json!(codex_model)
    };
    // 11 §7: `verified` needs shipped evidence for this exact connection.
    // Where this platform has none, the injected registry still passes all
    // four setup roles without consent, which is `verified_locally`. The
    // Claude row only ever reaches two roles (its fixture withholds
    // scoped_write), so it has nothing but shipped evidence to stand on, and
    // `with_binding_evidence` leaves the local self-check un-run.
    let codex_grade = if shipped_claims(RuntimeKind::Codex, codex_model, "0.154.0") {
        "verified"
    } else {
        "verified_locally"
    };
    let claude_grade = if shipped_claims(RuntimeKind::Claude, "opus", "2.1.270") {
        "verified"
    } else if capability_evidence::line_evidence_version(RuntimeKind::Claude, os, Some("2.1.270"))
        .is_some()
    {
        "same_line_unverified"
    } else {
        "unverified"
    };
    assert_eq!(
        result,
        json!({"runtimes":[
            {
                "runtime":"codex",
                "program":bin.join("codex").to_str().unwrap(),
                "version":"0.154.0",
                "installation":"verified",
                "login":"found",
                "configured_model_id":"gpt-configured",
                "suggested_provider_id":"openai",
                "proven_model_id":proven,
                "models":[{"id":"gpt-listed","efforts":["high"]},{"id":"gpt-configured","efforts":[]}],
                "verified_roles":["lead","builder","reviewer","integrator"],
                "grade":codex_grade,
                // The injected registry ignores consent: nothing more to enable.
                "experimental_roles":["lead","builder","reviewer","integrator"]
            },
            {
                "runtime":"claude",
                "program":bin.join("claude").to_str().unwrap(),
                "version":"2.1.270",
                "installation":"verified",
                "login":"found",
                "configured_model_id":"opus",
                "suggested_provider_id":"anthropic",
                "proven_model_id":null,
                "models":[{"id":"opus","efforts":[]}],
                "verified_roles":["lead","reviewer"],
                "grade":claude_grade,
                "experimental_roles":["lead","reviewer"]
            },
            {
                "runtime":"opencode",
                "program":"",
                "version":null,
                "installation":"not_found",
                "login":"unknown",
                "configured_model_id":null,
                "suggested_provider_id":"zai-coding-plan",
                "proven_model_id":null,
                // Never asked — not installed, and OpenCode is skipped even
                // when it is: a detection row cannot become its connection.
                // The stub's marker id would appear here if it had been.
                "models":[],
                "verified_roles":[],
                "grade":"not_installed",
                "experimental_roles":[]
            }
        ]})
    );
    // Detection is not a save or a probe of any stored binding.
    assert_eq!(
        call(&service, "binding.list", json!({}))["bindings"],
        json!([])
    );

    // Login markers and version failures are re-read on every call.
    std::fs::remove_file(home.join(".codex").join("auth.json")).unwrap();
    write(&home.join(".claude.json"), "{\"projects\":{}}");
    program(&bin.join("claude"), "printf 'not a version\\n'");
    let again = call(&service, "runtime.detect", json!({}));
    assert_eq!(again["runtimes"][0]["login"], "not_found");
    assert_eq!(again["runtimes"][1]["login"], "not_found");
    assert_eq!(again["runtimes"][1]["installation"], "unrecognized_version");
    assert_eq!(again["runtimes"][1]["version"], Value::Null);
    assert_eq!(again["runtimes"][1]["verified_roles"], json!([]));
    // Installed but the version is unknown: never verified, nothing to accept.
    assert_eq!(again["runtimes"][1]["grade"], "unverified");
    assert_eq!(again["runtimes"][1]["experimental_roles"], json!([]));
    // An unrecognized version is not a verified installation: no hints asked.
    assert_eq!(again["runtimes"][1]["models"], json!([]));
    assert_eq!(
        again["runtimes"][0]["models"][0]["id"],
        json!("gpt-listed"),
        "a verified CLI still offers its listing"
    );
    assert_eq!(
        again["runtimes"][1]["program"],
        json!(bin.join("claude").to_str().unwrap())
    );
}

#[test]
fn detection_offers_experimental_roles_from_the_production_registry_for_the_exact_version() {
    // Real registry (no injected claims): an unrecorded version has no
    // verified role, but consent for exactly that version passes every setup
    // role the adapter implements. OpenCode needs a saved connection, so a
    // detected subscription row offers nothing.
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let home = dir.path().join("home");
    program(&bin.join("codex"), "printf 'codex-cli 0.999.1\\n'");
    program(&bin.join("claude"), "printf '9.9.9 (Claude Code)\\n'");
    program(&bin.join("opencode"), "printf '9.9.9\\n'");
    write(
        &home.join(".codex").join("config.toml"),
        "model = \"gpt-configured\"\n",
    );
    write(
        &home.join(".claude").join("settings.json"),
        "{\"model\":\"opus\"}",
    );
    let storage = Arc::new(Storage::open(dir.path().join("state.db")).unwrap());
    let env = {
        let (bin, home) = (bin.clone(), home.clone());
        move || DetectionEnv {
            path: Some(bin.clone().into_os_string()),
            home: Some(home.clone()),
            ..DetectionEnv::default()
        }
    };
    let service = MissionService::new(
        storage.clone(),
        ArtifactStore::new(storage, dir.path().join("artifacts")),
    )
    .with_binding_evidence(
        |program, runtime| installation::observe(program, runtime).map(|found| found.version),
        capability_evidence::capabilities_for_binding,
    )
    .with_detection_env(env)
    .with_model_lister(stub_models);
    let result = call(&service, "runtime.detect", json!({}));
    let all = json!(["lead", "builder", "reviewer", "integrator"]);
    let os = std::env::consts::OS;
    assert_eq!(result["runtimes"][0]["verified_roles"], json!([]));
    assert_eq!(result["runtimes"][0]["experimental_roles"], all);
    // 11 §4 rule 2 / §7 rule 4: 0.999.1 sits on the 0.x line of this OS's
    // Codex fixture, but the shipped evidence is only carried onto it once
    // this version's own protocol self-check passes — and `with_binding_evidence`
    // leaves it un-run, so the row is "same line, confirm with a check now",
    // and not one role is verified.
    assert_eq!(
        result["runtimes"][0]["grade"],
        if capability_evidence::line_evidence_version(RuntimeKind::Codex, os, Some("0.999.1"))
            .is_some()
        {
            "same_line_unverified"
        } else {
            "unverified"
        }
    );
    assert_eq!(result["runtimes"][1]["verified_roles"], json!([]));
    assert_eq!(result["runtimes"][1]["experimental_roles"], all);
    // Claude 9.9.9 is a different major than any shipped fixture: no line.
    assert_eq!(result["runtimes"][1]["grade"], "unverified");
    if result["runtimes"][2]["installation"] == "verified" {
        assert_eq!(result["runtimes"][2]["experimental_roles"], json!([]));
    }
    // An installed OpenCode is still not asked: `stub_models` answers that
    // runtime with a marker id, so an empty row is proof it never ran.
    assert_eq!(result["runtimes"][2]["models"], json!([]));
    assert_eq!(
        call(&service, "binding.list", json!({}))["bindings"],
        json!([])
    );
}

#[test]
fn a_panicking_model_listing_empties_only_its_own_row() {
    // The hints are the last thing detection attaches and the only part that
    // leaves this process, so a listing is the one step of `runtime.detect`
    // that can fail in a way the caller did not ask about. The contract is
    // that it degrades to an empty candidate list for that row: everything
    // detection observed itself — installation, version, login, configured
    // model, roles, grade — is unaffected, and the other rows are untouched.
    //
    // A `join()`ed panic is returned as `Err`, never resumed, so nothing
    // propagates here; the harness captures the panic message of the scoped
    // thread and prints it only if this test fails.
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let home = dir.path().join("home");
    program(&bin.join("codex"), "printf 'codex-cli 0.154.0\\n'");
    program(&bin.join("claude"), "printf '2.1.270 (Claude Code)\\n'");
    write(&home.join(".codex").join("auth.json"), "{}");
    write(
        &home.join(".codex").join("config.toml"),
        "model = \"gpt-configured\"\n",
    );
    write(
        &home.join(".claude.json"),
        "{\"oauthAccount\":{\"accountUuid\":\"fixture\"}}",
    );
    write(
        &home.join(".claude").join("settings.json"),
        "{\"model\":\"opus\"}",
    );
    let storage = Arc::new(Storage::open(dir.path().join("state.db")).unwrap());
    let env = {
        let (bin, home) = (bin.clone(), home.clone());
        move || DetectionEnv {
            path: Some(bin.clone().into_os_string()),
            home: Some(home.clone()),
            ..DetectionEnv::default()
        }
    };
    let service = MissionService::new(
        storage.clone(),
        ArtifactStore::new(storage, dir.path().join("artifacts")),
    )
    .with_binding_evidence(
        |program, runtime| installation::observe(program, runtime).map(|found| found.version),
        capability_evidence::capabilities_for_binding,
    )
    .with_detection_env(env)
    .with_model_lister(panic_for_codex);

    let result = service
        .handle(&ConnectionId::generate(), "runtime.detect", &json!({}))
        .expect("a panicking listing is not an error for the call that asked")
        .result;
    let typed: RuntimeDetectResult = serde_json::from_value(result.clone()).unwrap();
    assert_eq!(typed.runtimes.len(), 3, "every row is still reported");

    assert_eq!(result["runtimes"][0]["runtime"], json!("codex"));
    assert_eq!(
        result["runtimes"][0]["models"],
        json!([]),
        "the panicking row offers nothing"
    );
    // Detection's own observations for that same row are untouched: a picker
    // hint that could not be fetched is not a failed detection.
    assert_eq!(result["runtimes"][0]["installation"], json!("verified"));
    assert_eq!(result["runtimes"][0]["version"], json!("0.154.0"));
    assert_eq!(result["runtimes"][0]["login"], json!("found"));
    assert_eq!(
        result["runtimes"][0]["configured_model_id"],
        json!("gpt-configured")
    );
    assert_eq!(
        result["runtimes"][0]["suggested_provider_id"],
        json!("openai")
    );
    assert!(
        result["runtimes"][0]["grade"].is_string(),
        "a grade is still decided for the row whose listing panicked"
    );

    // One thread's panic does not reach the others.
    assert_eq!(result["runtimes"][1]["runtime"], json!("claude"));
    assert_eq!(
        result["runtimes"][1]["models"],
        json!([{"id":"opus","efforts":["high"]}]),
        "a sibling listing still lands on the wire"
    );
    assert_eq!(result["runtimes"][1]["installation"], json!("verified"));
    assert_eq!(result["runtimes"][1]["version"], json!("2.1.270"));

    // OpenCode is not installed here, so its empty row is the usual one.
    assert_eq!(result["runtimes"][2]["installation"], json!("not_found"));
    assert_eq!(result["runtimes"][2]["models"], json!([]));

    // Detection is still not a save.
    assert_eq!(
        call(&service, "binding.list", json!({}))["bindings"],
        json!([])
    );
}

/// 11 §7: what this machine measures decides the row's grade. Claude Code
/// 9.9.9 is on no shipped line at all, so nothing but this PC's own self-check
/// can carry it — and when that check passes, the four setup roles pass
/// without a single consent.
#[test]
fn a_passing_local_self_check_verifies_a_row_this_pc_has_no_shipped_evidence_for() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let home = dir.path().join("home");
    program(&bin.join("claude"), "printf '9.9.9 (Claude Code)\\n'");
    write(
        &home.join(".claude.json"),
        "{\"oauthAccount\":{\"accountUuid\":\"fixture\"}}",
    );
    write(
        &home.join(".claude").join("settings.json"),
        "{\"model\":\"opus\"}",
    );
    let env = {
        let (bin, home) = (bin.clone(), home.clone());
        move || DetectionEnv {
            path: Some(bin.clone().into_os_string()),
            home: Some(home.clone()),
            ..DetectionEnv::default()
        }
    };
    assert!(
        capability_evidence::line_evidence_version(
            RuntimeKind::Claude,
            std::env::consts::OS,
            Some("9.9.9")
        )
        .is_none(),
        "the case is only meaningful while no shipped fixture reaches 9.9.9"
    );
    let detect = |name: &str, report: Option<LocalProbeReport>| -> Value {
        let storage = Arc::new(Storage::open(dir.path().join(name)).unwrap());
        let service = MissionService::new(
            storage.clone(),
            ArtifactStore::new(storage, dir.path().join(name).with_extension("artifacts")),
        )
        .with_binding_evidence(
            |program, runtime| installation::observe(program, runtime).map(|found| found.version),
            // The production projection: capabilities come from the layers,
            // and here the only layer with anything to say is the probe.
            capability_evidence::capabilities_for_binding,
        )
        .with_detection_env(env.clone())
        .with_model_lister(|_, _, _| Vec::new())
        .with_cheap_prober(move |runtime, program, model_id, _env, _budget| {
            assert_eq!(runtime, RuntimeKind::Claude);
            assert!(!program.is_empty(), "only a verified install is measured");
            assert_eq!(model_id, "opus", "the model this row's setup would save");
            report.clone()
        });
        let result = call(&service, "runtime.detect", json!({}));
        let row = result["runtimes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["runtime"] == "claude")
            .expect("every runtime is still reported")
            .clone();
        assert_eq!(row["version"], "9.9.9");
        assert_eq!(
            call(&service, "binding.list", json!({}))["bindings"],
            json!([]),
            "detection stores nothing, measured or not"
        );
        row
    };

    // Nothing measured here: an unrecorded version with no line is unverified,
    // exactly as before this ticket.
    let unmeasured = detect("unmeasured.db", None);
    assert_eq!(unmeasured["verified_roles"], json!([]));
    assert_eq!(unmeasured["grade"], "unverified");

    // Measured and passing: the adapter's flags are all there, so the daemon
    // knows this CLI does structured results, events, cancellation and both
    // workspace modes — the whole setup, with no shipped fixture and no
    // consent behind it.
    let measured = detect("measured.db", Some(cheap_report(true)));
    assert_eq!(
        measured["verified_roles"],
        json!(["lead", "builder", "reviewer", "integrator"])
    );
    assert_eq!(measured["grade"], "verified_locally");
    assert_eq!(
        measured["experimental_roles"],
        json!(["lead", "builder", "reviewer", "integrator"]),
        "consent adds nothing that was not already proved"
    );

    // Measured and failing: a self-check that ran and did not pass is not a
    // reason to claim anything, and consent cannot re-open what it disproved.
    let failed = detect("failed.db", Some(cheap_report(false)));
    assert_eq!(failed["verified_roles"], json!([]));
    assert_eq!(failed["grade"], "unverified");
}

/// Claude Code serves a second provider route: the same executable answers on
/// Z.ai Coding Plan (`ccg` launch profile, `claude-exec --provider zai`). The
/// row carries the ids its own transcripts show answering on that route, so a
/// team can pick GLM without the daemon hard-coding a model list.
#[test]
fn claude_row_carries_its_zai_route_candidates_from_its_transcripts() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    let home = dir.path().join("home");
    program(&bin.join("claude"), "printf '2.1.270 (Claude Code)\n'");
    write(
        &home.join(".claude.json"),
        "{\"oauthAccount\":{\"accountUuid\":\"fixture\"}}",
    );
    write(
        &home
            .join(".claude")
            .join("projects")
            .join("-Users-dev-alpha")
            .join("s1.jsonl"),
        "{\"type\":\"assistant\",\"message\":{\"model\":\"glm-5.3\",\"id\":\"m\"}}\n",
    );
    let storage = Arc::new(Storage::open(dir.path().join("state.db")).unwrap());
    let env = {
        let (bin, home) = (bin.clone(), home.clone());
        move || DetectionEnv {
            path: Some(bin.clone().into_os_string()),
            home: Some(home.clone()),
            ..DetectionEnv::default()
        }
    };
    let service = MissionService::new(
        storage.clone(),
        ArtifactStore::new(storage, dir.path().join("artifacts")),
    )
    .with_binding_evidence(
        |program, runtime| installation::observe(program, runtime).map(|found| found.version),
        |_, _, _| capability_evidence::unclaimed(),
    )
    .with_detection_env(env)
    .with_model_lister(|runtime, program, provider| {
        assert_eq!(runtime, RuntimeKind::Claude);
        assert_eq!(provider, Some("anthropic"));
        assert!(!program.is_empty());
        Vec::new()
    });

    let result = call(&service, "runtime.detect", json!({}));
    let typed: RuntimeDetectResult = serde_json::from_value(result).unwrap();
    let claude = typed
        .runtimes
        .iter()
        .find(|row| row.runtime == RuntimeKind::Claude)
        .unwrap();
    assert_eq!(
        claude.alt_models.as_deref().unwrap_or_default(),
        vec![RuntimeAltModels {
            provider_id: "zai-coding-plan".into(),
            models: vec![ProbeModel {
                id: "glm-5.3".into(),
                efforts: ["low", "medium", "high", "xhigh", "max"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            }],
        }]
    );
    assert!(typed
        .runtimes
        .iter()
        .find(|row| row.runtime == RuntimeKind::Codex)
        .unwrap()
        .alt_models
        .as_deref()
        .map_or(true, |models| models.is_empty()));
}
