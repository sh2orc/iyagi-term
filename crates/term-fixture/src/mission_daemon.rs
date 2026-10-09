//! Test-only daemon executable. Never bundled: server evidence is limited to
//! the explicitly supplied deterministic protocol fixture, not any real CLI.
use clap::Parser;
use iyagi_termd_lib::{
    agent_runtime::{capability_evidence, fake::fake_binding, installation::ProbeFailure},
    mission::{artifacts::ArtifactStore, MissionService},
    Daemon,
};
use std::{path::PathBuf, sync::Arc};
use term_contracts::mission::types::{AuthRoute, Id, RuntimeKind};

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    protocol_fixture: Option<PathBuf>,
    #[arg(long, num_args = 2)]
    launch_helper: Option<Vec<String>>,
    #[arg(long, num_args = 2)]
    exec_guardian: Option<Vec<String>>,
    #[arg(long, num_args = 2)]
    integration_helper: Option<Vec<String>>,
}
fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    if let Some(args) = cli.launch_helper {
        std::process::exit(iyagi_termd_lib::helper::run(&args[0], &args[1]));
    }
    if let Some(args) = cli.integration_helper {
        std::process::exit(iyagi_termd_lib::mission::integration_exec::helper(
            std::path::Path::new(&args[0]),
            &args[1],
        ));
    }
    if let Some(args) = cli.exec_guardian {
        #[cfg(target_os = "macos")]
        {
            let id = term_contracts::ids::WorkloadId::parse(&args[1]).unwrap();
            std::process::exit(
                if term_platform::group::macos_guardian::serve(&args[0], &id).is_ok() {
                    0
                } else {
                    7
                },
            );
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = args;
            std::process::exit(7);
        }
    }
    let fixture = cli
        .protocol_fixture
        .expect("explicit protocol fixture required");
    assert!(fixture.is_absolute() && fixture.is_file());
    let fixture = fixture.to_string_lossy().into_owned();
    let probe_program = fixture.clone();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut daemon = Daemon::start(
        cli.data_dir.expect("test data directory required"),
        runtime.handle().clone(),
    )
    .unwrap();
    let state = Arc::get_mut(&mut daemon.state).unwrap();
    state.missions = Some(Arc::new(
        MissionService::new(
            state.storage.clone(),
            ArtifactStore::new(state.storage.clone(), state.paths.missions_dir()),
        )
        .with_daemon_id(Id::parse(&state.daemon_id).unwrap())
        .with_binding_evidence(
            move |program, kind| {
                if program == probe_program && kind == RuntimeKind::Codex {
                    Ok("protocol-fixture-v1".into())
                } else {
                    Err(ProbeFailure::NotFound)
                }
            },
            move |binding, _, version| {
                if binding.program == fixture
                    && binding.runtime == RuntimeKind::Codex
                    && binding.provider_id == "openai"
                    && binding.auth_route == AuthRoute::Subscription
                    && binding.credential_ref.is_none()
                    && binding.endpoint_ref.is_none()
                    && version == Some("protocol-fixture-v1")
                {
                    let mut caps = fake_binding().capabilities;
                    caps.steer.supported = true;
                    caps.steer.reason_code = None;
                    caps
                } else {
                    capability_evidence::unclaimed()
                }
            },
        ),
    ));
    let code = runtime.block_on(daemon.run());
    runtime.shutdown_background();
    std::process::exit(code);
}
