//! # term-fixture
//!
//! Deterministic load programs used by backend/UI acceptance tests
//! (spec `06-verification.md` §2). Calls no LLM and touches only its given
//! temp working directory. Modes: echo, flood, tui, memory, cpu, tree,
//! escape, side-effect, gate-observer, io, exit, agent-fake.

mod app_server;
mod claude_print;
mod modes;
mod opencode_server;
mod rng;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "term-fixture",
    about = "Deterministic terminal test programs (never shipped)"
)]
struct Cli {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand)]
enum Mode {
    /// Deterministic Codex app-server protocol fixture; no model calls.
    AppServer {
        #[arg(short = 'c', long = "config")]
        config: Vec<String>,
    },
    /// Deterministic authenticated OpenCode HTTP/SSE fixture; no model calls.
    Serve {
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        hostname: String,
        #[arg(long)]
        pure: bool,
    },
    /// Echo raw stdin bytes back on stdout (UTF-8 split / paste comparison).
    Echo,
    /// Emit a deterministic byte pattern; report SHA-256 + total on stderr.
    Flood {
        #[arg(long)]
        bytes: u64,
        #[arg(long, default_value_t = 4096)]
        chunk: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// 출력을 전부 쓴 뒤 종료까지 대기(밀리초) — 살아 있는 세션의
        /// detach/re-attach 재생을 재는 벤치가 쓴다.
        #[arg(long, default_value_t = 0)]
        hold_ms: u64,
    },
    /// ANSI cursor/erase/colors/alternate screen/queries with seeded variation.
    Tui {
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
    /// Allocate pages and touch them so RSS actually grows, hold, release.
    Memory {
        #[arg(long)]
        mib: usize,
        #[arg(long, default_value_t = 1000)]
        hold_ms: u64,
    },
    /// N busy workers for a fixed duration (CPU contention).
    Cpu {
        #[arg(long, default_value_t = 1)]
        workers: usize,
        #[arg(long, default_value_t = 2000)]
        duration_ms: u64,
    },
    /// Spawn a depth x breadth child tree that holds; optional early root exit
    /// leaves descendants behind (root-exit / group-cancel checks).
    Tree {
        #[arg(long, default_value_t = 2)]
        children: usize,
        #[arg(long, default_value_t = 1)]
        depth: usize,
        #[arg(long, default_value_t = 1000)]
        hold_ms: u64,
        #[arg(long)]
        root_early_exit_ms: Option<u64>,
    },
    /// Unix: detach into a new session (observation limits). Windows: note.
    Escape,
    /// Exclusive file creation (O_EXCL semantics) — duplicate-run detector.
    SideEffect {
        #[arg(long)]
        file: PathBuf,
    },
    /// Write a marker file at process start; gate RELEASE-order invariant.
    GateObserver {
        #[arg(long)]
        marker: PathBuf,
    },
    /// Dedicated temp-file write+read loop with a rate report on stderr.
    Io {
        #[arg(long)]
        bytes: u64,
        #[arg(long)]
        file: PathBuf,
    },
    /// Exit with `code` after `delay_ms` (success/failure/cancel races).
    Exit {
        #[arg(long, default_value_t = 0)]
        code: i32,
        #[arg(long, default_value_t = 0)]
        delay_ms: u64,
    },
    /// O07 fake-adapter scenario player: replay a scripted agent run on
    /// stdout as JSONL protocol lines (docs/orchestration/03-adapters.md §7).
    AgentFake {
        /// Scenario JSON, base64-encoded (argv-safe).
        scenario: String,
    },
}

fn main() {
    // Accept the real print adapter's fixed argv without a shell wrapper.
    // This deterministic transport fixture performs no provider inference.
    let argv: Vec<_> = std::env::args().skip(1).collect();
    if argv.first().is_some_and(|arg| arg == "-p") {
        std::process::exit(claude_print::run(&argv));
    }
    let cli = Cli::parse();
    let code = match cli.mode {
        Mode::AppServer { config } => app_server::run(&config),
        Mode::Serve {
            port,
            hostname,
            pure: _,
        } => match opencode_server::run(&hostname, port) {
            Ok(()) => 0,
            Err(_) => 1,
        },
        Mode::Echo => modes::echo(),
        Mode::Flood {
            bytes,
            chunk,
            seed,
            hold_ms,
        } => modes::flood(bytes, chunk, seed, hold_ms),
        Mode::Tui { seed } => modes::tui(seed),
        Mode::Memory { mib, hold_ms } => modes::memory(mib, hold_ms),
        Mode::Cpu {
            workers,
            duration_ms,
        } => modes::cpu(workers, duration_ms),
        Mode::Tree {
            children,
            depth,
            hold_ms,
            root_early_exit_ms,
        } => modes::tree(children, depth, hold_ms, root_early_exit_ms),
        Mode::Escape => modes::escape(),
        Mode::SideEffect { file } => modes::side_effect(&file),
        // Marker first, everything else after — the marker IS the signal.
        Mode::GateObserver { marker } => modes::gate_observer(&marker),
        Mode::Io { bytes, file } => modes::io(bytes, &file),
        Mode::Exit { code, delay_ms } => modes::exit(code, delay_ms),
        Mode::AgentFake { scenario } => modes::agent_fake(&scenario),
    };
    std::process::exit(code);
}
