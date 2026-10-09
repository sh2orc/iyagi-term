//! # iyagi-termd
//!
//! User-privileged execution daemon shipped with the app (never a system
//! service). Owns PTYs and workload groups independently of any window.
//!
//! Modes:
//! * default — daemon: migrations → crash reconciliation → IPC listener → ready
//! * `--launch-helper` — gated first child of a managed PTY (spec 02 §3)

use clap::{Parser, Subcommand};

/// `claude-exec` subcommand. Bin-crate only: it runs one Claude Code process
/// and exits, so the daemon library never links it.
mod claude_exec;

#[derive(Debug, Subcommand)]
enum Command {
    /// CLI hook gateway (SOTA_GAP_REVIEW W1-5, spec 02-runner §8): reads a
    /// hook JSON payload from stdin and reports it to the running daemon as
    /// an intervention notice or an agent-session lifecycle event.
    Hook {
        /// Which CLI registered this hook ("claude" | "codex" | "opencode"). Decides the
        /// reported agent id and source label.
        #[arg(long, default_value = "claude")]
        agent: String,
    },
    /// Capture Claude Code subscription rate-limit fields for iyagi's
    /// local usage display, then preserve the user's original status line.
    ClaudeUsage,
    /// Provision or inspect orchestration provider connections locally.
    Connection {
        #[command(subcommand)]
        command: iyagi_termd_lib::connections::ConnectionCommand,
    },
    /// Run Claude Code with iyagi's provider routing, then hand this
    /// process over to it (outside a terminal pane).
    ClaudeExec {
        /// Provider routing to apply.
        #[arg(long, value_parser = ["zai", "anthropic"])]
        provider: String,
        /// Z.ai main model for the opus/sonnet slots. Defaults to glm-5.3[1m].
        #[arg(long, value_name = "ID")]
        main_model: Option<String>,
        /// Claude Code executable. Defaults to the first `claude` on PATH.
        #[arg(long, value_name = "PATH")]
        program: Option<std::path::PathBuf>,
        /// Arguments forwarded to Claude Code (use `--` before flags).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// `--version`에 빌드 id(`ipc::build_version`)를 그대로 싣는다. 앱은 연결된
/// 데몬의 hello `daemon_version`을 자기 빌드 id뿐 아니라 디스크의 데몬
/// 바이너리가 보고하는 이 값과도 비교해, 다시 빌드했지만 옛 프로세스가 아직
/// 돌고 있는 데몬을 "오래됨"으로 잡는다(수정이 반영되지 않은 채 계속 도는
/// 개발 함정).
fn cli_version() -> &'static str {
    Box::leak(iyagi_termd_lib::ipc::build_version().into_boxed_str())
}

#[derive(Debug, Parser)]
#[command(
    name = "iyagi-termd",
    about = "iyagi execution daemon",
    version = cli_version(),
    disable_help_subcommand = true
)]
struct Cli {
    /// Run as the gated launch helper (spec 02-runner §3).
    #[arg(long, num_args = 2, value_names = ["ENDPOINT", "NONCE"])]
    launch_helper: Option<Vec<String>>,

    #[arg(long, num_args = 2, value_names = ["ENDPOINT", "WORKLOAD"], hide = true)]
    exec_guardian: Option<Vec<String>>,

    #[arg(long, num_args = 2, value_names = ["INPUT", "SHA256"], hide = true)]
    integration_helper: Option<Vec<String>>,

    #[command(subcommand)]
    command: Option<Command>,

    /// Data directory (config/data/runtime tree). Defaults to the platform
    /// local-data dir + Iyagi.
    #[arg(long, global = true)]
    data_dir: Option<std::path::PathBuf>,
}

/// Initialize tracing. Always writes to stderr (unchanged from before). When
/// `enable_file` is set (the long-running daemon), it ALSO writes a
/// daily-rolling `iyagi-termd.log` under `<data_dir>/logs/`, so the daemon
/// keeps a persistent record even though its stderr is nulled once detached.
///
/// The returned `WorkerGuard` flushes the non-blocking file writer on drop and
/// MUST be held for the whole process lifetime — dropping it stops file
/// logging. Falls back to stderr-only (returns `None`) when the log directory
/// can't be created, so a bad/unwritable data dir never blocks startup.
fn init_logging(
    data_dir: &std::path::Path,
    enable_file: bool,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "iyagi_termd_lib=info,iyagi_termd=info".into());
    let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    let mut file_dir_error: Option<(std::path::PathBuf, std::io::Error)> = None;
    let (file_layer, guard) = if enable_file {
        let logs_dir = data_dir.join("logs");
        match std::fs::create_dir_all(&logs_dir) {
            Ok(()) => {
                let appender = tracing_appender::rolling::daily(&logs_dir, "iyagi-termd.log");
                let (non_blocking, guard) = tracing_appender::non_blocking(appender);
                let layer = tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(non_blocking);
                (Some(layer), Some(guard))
            }
            Err(error) => {
                file_dir_error = Some((logs_dir, error));
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    // `Option<Layer>` is itself a `Layer` (a no-op when `None`), so one
    // registry builds with or without the file sink.
    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();

    if let Some((dir, error)) = file_dir_error {
        tracing::warn!(
            %error,
            dir = %dir.display(),
            "log directory unavailable; logging to stderr only"
        );
    }

    guard
}

fn main() {
    let cli = Cli::parse();

    // Resolve the data dir exactly as the daemon does (below), up front, so
    // the persistent log file can live under it. `default_data_dir` is
    // infallible.
    let data_dir = cli
        .data_dir
        .clone()
        .unwrap_or_else(iyagi_termd_lib::paths::default_data_dir);

    // Only the long-running daemon keeps a persistent log file. The short-lived
    // helper/guardian/CLI invocations stay stderr-only: they run one-shot
    // (often inside a managed PTY), so a rolling file + worker thread would
    // litter the data dir for no benefit.
    let is_daemon = cli.command.is_none()
        && cli.launch_helper.is_none()
        && cli.exec_guardian.is_none()
        && cli.integration_helper.is_none();

    // Held for the whole process: dropping the guard stops the non-blocking
    // file writer. The daemon path keeps it alive until `std::process::exit`.
    let _log_guard = init_logging(&data_dir, is_daemon);

    if let Some(args) = cli.integration_helper {
        std::process::exit(iyagi_termd_lib::mission::integration_exec::helper(
            std::path::Path::new(&args[0]),
            &args[1],
        ));
    }

    if let Some(args) = cli.launch_helper {
        let endpoint = args.first().map(String::as_str).unwrap_or_default();
        let nonce = args.get(1).map(String::as_str).unwrap_or_default();
        std::process::exit(iyagi_termd_lib::helper::run(endpoint, nonce));
    }
    if let Some(args) = cli.exec_guardian {
        #[cfg(target_os = "macos")]
        {
            let workload = term_contracts::ids::WorkloadId::parse(&args[1]);
            let result = workload
                .map_err(|e| std::io::Error::other(e.to_string()))
                .and_then(|id| term_platform::group::macos_guardian::serve(&args[0], &id));
            std::process::exit(match result {
                Ok(()) => 0,
                Err(error) => {
                    tracing::error!(%error, "observer guardian failed");
                    7
                }
            });
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = args;
            std::process::exit(7);
        }
    }

    if let Some(command) = cli.command {
        let code = match command {
            Command::Hook { agent } => iyagi_termd_lib::hook::run(&data_dir, &agent),
            Command::ClaudeUsage => iyagi_termd_lib::claude_usage::run(&data_dir),
            Command::Connection { command } => {
                iyagi_termd_lib::connections::run_cli(&data_dir, command)
            }
            // On unix this replaces the process on success and never returns.
            Command::ClaudeExec {
                provider,
                main_model,
                program,
                args,
            } => claude_exec::run(&data_dir, &provider, main_model.as_deref(), program, &args),
        };
        std::process::exit(code);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let code = match iyagi_termd_lib::Daemon::start(data_dir, runtime.handle().clone()) {
        Ok(daemon) => runtime.block_on(daemon.run()),
        Err(code) => code,
    };
    // Storage cleanup (writer thread join) happens on drop; force it before
    // exit so the WAL is consistent.
    runtime.shutdown_background();
    std::process::exit(code);
}
