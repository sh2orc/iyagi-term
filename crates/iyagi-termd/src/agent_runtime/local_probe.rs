//! # Local compatibility self-check (docs/orchestration/11-local-evidence.md §6)
//!
//! Moves verification from the author's machine to the user's: instead of
//! trusting a recorded fixture for one pinned CLI build, the daemon asks the
//! installed CLI itself, on this OS, what it supports — without inference.
//!
//! Hard rules for everything in this module:
//! * no model call, no authentication token, no user configuration change;
//! * every child is reaped on every path, including timeouts and errors;
//! * each step carries its own deadline and the whole probe stays inside
//!   `budget` ([`BUDGET`] by default);
//! * failures are short fixed slugs — raw CLI output never reaches a report,
//!   a log line, or the wire.
//!
//! The report is pure data ([`LocalProbeReport`]); [`capabilities`] maps it to
//! the evidence-only [`RuntimeCapabilities`] the registry layers in
//! (`capability_evidence`, 11 §4). Report builders and parsers are separated
//! from process handling so they can be unit-tested without spawning anything.

use std::io::BufRead;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use term_contracts::mission::types::{
    Binding, LocalProbeReport, RuntimeCapabilities, RuntimeKind, Support,
};

use super::capability_evidence::{unclaimed, LOCAL_PROBE_FAILED};
use super::codex::{LivePeer, PeerEvent, ProtocolPeer};
use super::detection::DetectionEnv;
use super::installation;

/// Whole-probe budget for `binding.probe` (11 §6). `runtime.detect` passes a
/// much smaller one: its rows run in parallel, so one row's wait is the
/// user-visible wait.
pub const BUDGET: Duration = Duration::from_secs(12);
/// `runtime.detect` budget per row: the rows run in parallel and nothing is
/// stored, so a CLI that does not answer its handshake promptly is simply
/// reported without a probe (grade falls back to the shipped/line evidence).
pub const DETECT_BUDGET: Duration = Duration::from_secs(2);

/// `codex app-server` argv — mirrors `codex::APP_SERVER_ARGV` (private to the
/// adapter module); the probe must speak to the same server the adapter does.
const APP_SERVER_ARGV: [&str; 1] = ["app-server"];

/// OpenCode server argv — mirrors `opencode::LiveServerPlan::for_binding`.
const OPENCODE_SERVE_ARGV: [&str; 6] =
    ["serve", "--pure", "--port", "0", "--hostname", "127.0.0.1"];

// ---- fixed failure slugs ----------------------------------------------------

/// The CLI did not complete the adapter's protocol handshake.
pub const FAIL_HANDSHAKE: &str = "handshake";
/// `claude --help` could not be read at all (missing, timed out, over cap).
pub const FAIL_HELP_UNAVAILABLE: &str = "help_unavailable";
/// OpenCode needs a saved connection before anything can be checked.
pub const FAIL_CONNECTION_REQUIRED: &str = "connection_required";
/// The owned server did not start, authenticate, or report itself healthy.
pub const FAIL_SERVER_UNAVAILABLE: &str = "server_unavailable";
/// The running server reports a different version than the installation check.
pub const FAIL_VERSION_CHANGED: &str = "version_changed";
/// `flag_missing:<flag>` — the CLI's help does not advertise a flag the
/// adapter passes.
pub const FLAG_MISSING: &str = "flag_missing:";
/// `sandbox:<case>` — an OS boundary case the adapter relies on did not hold.
pub const SANDBOX_CASE: &str = "sandbox:";
/// `route_missing:<name>` — the served OpenAPI document lacks an adapter route.
pub const ROUTE_MISSING: &str = "route_missing:";

/// Flags the Claude print adapter passes or depends on; all of them must be
/// advertised by the installed CLI (03 §4, 11 §6).
pub const CLAUDE_FLAGS: [&str; 9] = [
    "--output-format",
    "--json-schema",
    "--permission-mode",
    "--allowedTools",
    "--disallowedTools",
    "--verbose",
    "--setting-sources",
    "--strict-mcp-config",
    "--model",
];

/// Routes the OpenCode adapter posts to, with the short slug a missing one
/// reports. The path templates are the served OpenAPI ones (`opencode/mod.rs`).
pub const OPENCODE_ROUTES: [(&str, &str); 5] = [
    ("/session", "session_create"),
    ("/session/{sessionID}/prompt_async", "session_prompt"),
    ("/session/{sessionID}/abort", "session_abort"),
    ("/permission/{requestID}/reply", "permission_reply"),
    ("/event", "event_stream"),
];

/// Bounded `claude --help` capture (the installed help is a few KiB).
const HELP_LIMIT: usize = 256 * 1024;
/// Per-step ceiling; the remaining budget can always shorten it.
const STEP_TIMEOUT: Duration = Duration::from_secs(5);
/// `model/list` pages followed before the probe gives up (same cap as the
/// adapter's catalog read).
const MAX_MODEL_PAGES: usize = 8;
/// Content every sandbox write case starts from; a case "changed" the file
/// when its bytes differ afterwards.
const SANDBOX_ORIGINAL: &[u8] = b"sandbox-probe-original";

// ---- public API -------------------------------------------------------------

/// Full self-check for one binding. Never panics and never returns an error:
/// a probe that could not run is a report that proves nothing.
pub fn run(
    binding: &Binding,
    program: &str,
    version: &str,
    env: &DetectionEnv,
    budget: Duration,
) -> LocalProbeReport {
    let deadline = Instant::now() + budget;
    match binding.runtime {
        RuntimeKind::Codex => codex_probe(program, &binding.model_id, env, deadline, true),
        RuntimeKind::Claude => claude_probe(program, deadline),
        RuntimeKind::Opencode => opencode_probe(binding, program, version, env, deadline),
        RuntimeKind::Fake => empty_report(),
    }
}

/// The cheap part `runtime.detect` can afford per row: the Codex handshake
/// plus `model/list`, and the Claude help read. OpenCode needs an owned
/// server (and a saved connection), so detection skips it.
pub fn run_cheap(
    runtime: RuntimeKind,
    program: &str,
    model_id: &str,
    env: &DetectionEnv,
    budget: Duration,
) -> Option<LocalProbeReport> {
    let deadline = Instant::now() + budget;
    match runtime {
        RuntimeKind::Codex => Some(codex_probe(program, model_id, env, deadline, false)),
        RuntimeKind::Claude => Some(claude_probe(program, deadline)),
        RuntimeKind::Opencode | RuntimeKind::Fake => None,
    }
}

/// 11 §6 mapping: report → evidence-only capabilities. `supported` carries no
/// reason (the registry labels the layer); a capability the check disproved is
/// [`LOCAL_PROBE_FAILED`] and consent can never re-open it; everything else
/// stays unclaimed.
pub fn capabilities(runtime: RuntimeKind, report: &LocalProbeReport) -> RuntimeCapabilities {
    let mut capabilities = unclaimed();
    match runtime {
        RuntimeKind::Codex => {
            if !report.protocol_ok {
                return capabilities;
            }
            // The app-server completed `initialize`: schema-constrained
            // results, the notification stream, interrupt, steer, approval
            // replies and usage all ride on that one connection.
            capabilities.structured_result = proven();
            capabilities.events = proven();
            capabilities.cancel = proven();
            capabilities.steer = proven();
            capabilities.approval_reply = proven();
            capabilities.usage = proven();
            if report.model_listed.is_some() {
                capabilities.model_listing = proven();
            }
            // Workspace boundaries are an OS fact, proven case by case.
            let broken = report
                .failures
                .iter()
                .any(|failure| failure.starts_with(SANDBOX_CASE));
            let complete = matches!(
                (report.sandbox_cases_passed, report.sandbox_cases_total),
                (Some(passed), Some(total)) if total > 0 && passed == total
            );
            if broken {
                capabilities.read_only = disproven();
                capabilities.scoped_write = disproven();
            } else if complete {
                capabilities.read_only = proven();
                capabilities.scoped_write = proven();
            }
        }
        RuntimeKind::Claude => {
            let missing: Vec<&str> = report
                .failures
                .iter()
                .filter_map(|failure| failure.strip_prefix(FLAG_MISSING))
                .collect();
            // Either every flag was advertised or the help listed which ones
            // were not: both mean the help text itself was read. A report
            // without that evidence proves nothing either way.
            if !report.protocol_ok && missing.is_empty() {
                return capabilities;
            }
            let advertised = |flag: &str| !missing.contains(&flag);
            let from = |ok: bool| if ok { proven() } else { disproven() };
            capabilities.structured_result = from(advertised("--json-schema"));
            capabilities.events = from(advertised("--output-format"));
            capabilities.usage = from(advertised("--output-format"));
            // Cancellation is the daemon's supervised stop ladder, not a CLI
            // flag: an installed print-mode CLI always has it.
            capabilities.cancel = proven();
            let permissions = advertised("--permission-mode")
                && advertised("--allowedTools")
                && advertised("--disallowedTools");
            capabilities.read_only = from(permissions);
            capabilities.scoped_write = from(permissions);
        }
        RuntimeKind::Opencode => {
            if !report.protocol_ok {
                return capabilities;
            }
            capabilities.structured_result = proven();
            capabilities.events = proven();
            capabilities.cancel = proven();
            capabilities.approval_reply = proven();
            capabilities.usage = proven();
            capabilities.read_only = proven();
            capabilities.scoped_write = proven();
            if report.model_listed.is_some() {
                capabilities.model_listing = proven();
            }
        }
        RuntimeKind::Fake => {}
    }
    capabilities
}

fn proven() -> Support {
    Support {
        supported: true,
        reason_code: None,
    }
}

fn disproven() -> Support {
    Support {
        supported: false,
        reason_code: Some(LOCAL_PROBE_FAILED.into()),
    }
}

/// A report that claims nothing (the fake runtime, or a probe that never ran).
pub fn empty_report() -> LocalProbeReport {
    LocalProbeReport {
        protocol_ok: false,
        sandbox_cases_passed: None,
        sandbox_cases_total: None,
        model_listed: None,
        failures: Vec::new(),
    }
}

fn failed_report(slug: &str) -> LocalProbeReport {
    LocalProbeReport {
        failures: vec![slug.to_owned()],
        ..empty_report()
    }
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn step_timeout(deadline: Instant) -> Duration {
    remaining(deadline).min(STEP_TIMEOUT)
}

// ---- Claude: advertised flags ------------------------------------------------

/// Pure parser over captured `claude --help` text. Every flag the adapter
/// passes must be advertised; the ones that are not become fixed
/// `flag_missing:<flag>` slugs (never a copy of the CLI's own text).
pub fn claude_help_report(help_text: &str) -> LocalProbeReport {
    if help_text.trim().is_empty() {
        return failed_report(FAIL_HELP_UNAVAILABLE);
    }
    let missing: Vec<&str> = CLAUDE_FLAGS
        .iter()
        .copied()
        .filter(|flag| !advertises(help_text, flag))
        .collect();
    LocalProbeReport {
        protocol_ok: missing.is_empty(),
        failures: missing
            .iter()
            .map(|flag| format!("{FLAG_MISSING}{flag}"))
            .collect(),
        ..empty_report()
    }
}

/// Whole-token flag match: `--allowed-tools` must not satisfy `--allowedTools`,
/// and `--output-format=stream-json` must satisfy `--output-format`.
fn advertises(help_text: &str, flag: &str) -> bool {
    let bytes = help_text.as_bytes();
    let boundary = |byte: u8| !(byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    let mut from = 0usize;
    while let Some(found) = help_text[from..].find(flag) {
        let start = from + found;
        let end = start + flag.len();
        let before_ok = start == 0 || boundary(bytes[start - 1]);
        let after_ok = end >= bytes.len() || boundary(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

fn claude_probe(program: &str, deadline: Instant) -> LocalProbeReport {
    match installation::capture_stdout(program, &["--help"], HELP_LIMIT, step_timeout(deadline)) {
        Ok(help_text) => claude_help_report(&help_text),
        Err(_) => failed_report(FAIL_HELP_UNAVAILABLE),
    }
}

// ---- Codex: app-server handshake, model list, sandbox boundaries --------------

/// Pure report builder for the Codex row so the mapping is testable without a
/// process. `sandbox` is `None` when the boundary cases were not attempted
/// (Windows, `runtime.detect`, or an inconclusive run).
pub fn codex_report(
    handshake_ok: bool,
    model_listed: Option<bool>,
    sandbox: Option<&[(&str, bool)]>,
) -> LocalProbeReport {
    let mut report = LocalProbeReport {
        protocol_ok: handshake_ok,
        model_listed,
        ..empty_report()
    };
    if !handshake_ok {
        report.failures.push(FAIL_HANDSHAKE.to_owned());
    }
    if let Some(cases) = sandbox {
        let passed = cases.iter().filter(|(_, passed)| *passed).count();
        report.sandbox_cases_total = Some(cases.len() as u32);
        report.sandbox_cases_passed = Some(passed as u32);
        for (case, passed) in cases {
            if !*passed {
                report.failures.push(format!("{SANDBOX_CASE}{case}"));
            }
        }
    }
    report
}

#[derive(Debug, Default)]
struct CodexSession {
    handshake_ok: bool,
    model_listed: Option<bool>,
    sandbox: Option<Vec<(&'static str, bool)>>,
}

fn codex_probe(
    program: &str,
    model_id: &str,
    env: &DetectionEnv,
    deadline: Instant,
    with_sandbox: bool,
) -> LocalProbeReport {
    let Ok(workspace) = tempfile::Builder::new()
        .prefix("iyagi-codex-probe-")
        .tempdir()
    else {
        return failed_report(FAIL_HANDSHAKE);
    };
    let plan = with_sandbox.then(|| sandbox_plan(env)).flatten();
    let cwd = plan
        .as_ref()
        .map(|plan| plan.workspace.clone())
        .unwrap_or_else(|| workspace.path().to_path_buf());
    let mut argv: Vec<String> = super::codex::argv_prefix(Path::new(program));
    argv.extend(APP_SERVER_ARGV.iter().map(|arg| (*arg).to_owned()));
    let Ok(peer) = LivePeer::spawn(Path::new(program), &argv, &cwd) else {
        return failed_report(FAIL_HANDSHAKE);
    };
    // `recv` has no deadline of its own, so the session runs on its own
    // thread and this one enforces the budget. The worker is deliberately
    // detached: on a timeout it is parked in `recv`, and `close()` below is
    // what drops stdin, reaps the child, and unblocks it.
    let (report, results) = mpsc::channel();
    let worker = Arc::clone(&peer);
    let model = model_id.to_owned();
    let spawned = std::thread::Builder::new()
        .name("codex-local-probe".into())
        .spawn(move || {
            let _ = report.send(codex_session(worker.as_ref(), &model, plan, deadline));
        });
    let session = match spawned {
        Ok(_) => results
            .recv_timeout(remaining(deadline))
            .unwrap_or_default(),
        Err(_) => CodexSession::default(),
    };
    // Always reap, on both the timeout and the normal path.
    peer.close();
    codex_report(
        session.handshake_ok,
        session.model_listed,
        session.sandbox.as_deref(),
    )
}

/// Pure over the transport: `initialize` → `initialized` → `model/list` →
/// optional `command/exec` boundary cases. Nothing here starts a thread or a
/// turn, so no model is ever called and no approval can arrive.
fn codex_session(
    peer: &dyn ProtocolPeer,
    model_id: &str,
    plan: Option<SandboxPlan>,
    deadline: Instant,
) -> CodexSession {
    let mut session = CodexSession::default();
    let mut next_id: u64 = 1;
    let Ok(response) = request(
        peer,
        next_id,
        "initialize",
        json!({
            "clientInfo": {
                "name": "iyagi",
                "title": null,
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {},
        }),
    ) else {
        return session;
    };
    if response.get("error").is_some() || !response.get("result").is_some_and(Value::is_object) {
        return session;
    }
    session.handshake_ok = true;
    if notify(peer, "initialized", json!({})).is_err() {
        return session;
    }
    session.model_listed = codex_model_listed(peer, &mut next_id, model_id, deadline);
    if let Some(plan) = plan {
        session.sandbox = codex_sandbox_cases(peer, &mut next_id, &plan, deadline);
    }
    session
}

/// `Some(true)` when the bound model is advertised, `Some(false)` when the
/// listing was read and does not contain it, `None` when there is no listing.
fn codex_model_listed(
    peer: &dyn ProtocolPeer,
    next_id: &mut u64,
    model_id: &str,
    deadline: Instant,
) -> Option<bool> {
    let mut cursor: Option<String> = None;
    let mut listed = false;
    let mut read_any = false;
    for _ in 0..MAX_MODEL_PAGES {
        if remaining(deadline).is_zero() {
            break;
        }
        let mut params = json!({ "includeHidden": false });
        if let Some(cursor) = cursor.as_deref() {
            params["cursor"] = Value::String(cursor.to_owned());
        }
        *next_id += 1;
        let Ok(response) = request(peer, *next_id, "model/list", params) else {
            break;
        };
        if response.get("error").is_some() {
            break;
        }
        let Some(entries) = response["result"]["data"].as_array() else {
            break;
        };
        read_any = true;
        listed = listed
            || entries
                .iter()
                .any(|entry| advertises_model(entry, model_id));
        if listed {
            break;
        }
        cursor = response["result"]["nextCursor"]
            .as_str()
            .filter(|cursor| !cursor.is_empty())
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    read_any.then_some(listed)
}

/// `model` first (the id `thread/start` takes), then `id`; hidden entries are
/// not selectable and never count.
fn advertises_model(entry: &Value, model_id: &str) -> bool {
    if entry.get("hidden").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    [entry.get("model"), entry.get("id")]
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|candidate| candidate == model_id)
}

// ---- Codex sandbox boundary cases (port of scripts/codex_sandbox_probe.py) ----

struct WriteCase {
    case: &'static str,
    target: PathBuf,
    writable: bool,
    expected: bool,
}

struct SandboxPlan {
    workspace: PathBuf,
    cases: Vec<WriteCase>,
    /// Absolute `python3` used only by the two network cases.
    python: Option<String>,
    listener: Option<TcpListener>,
    /// Probe-owned directories; dropping the plan removes them.
    _root: tempfile::TempDir,
    _slash_tmp: tempfile::TempDir,
}

/// Build the whole boundary layout or nothing: a partially prepared probe
/// would silently shrink the case count and weaken what "all passed" means.
fn sandbox_plan(env: &DetectionEnv) -> Option<SandboxPlan> {
    if !cfg!(unix) {
        return None;
    }
    // `_root` lives inside TMPDIR, so `root/tmp/...` exercises the
    // `excludeTmpdirEnvVar` boundary the same way the recorded probe did.
    let root = tempfile::Builder::new()
        .prefix("iyagi-codex-sandbox-")
        .tempdir()
        .ok()?;
    let slash_tmp = tempfile::Builder::new()
        .prefix("iyagi-codex-slash-tmp-")
        .tempdir_in("/tmp")
        .ok()?;
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside");
    for directory in [
        workspace.clone(),
        outside.clone(),
        workspace.join(".git"),
        workspace.join(".codex"),
        workspace.join(".agents"),
        root.path().join("tmp"),
    ] {
        std::fs::create_dir_all(&directory).ok()?;
    }
    link_dir(&outside, &workspace.join("external-link")).ok()?;
    let cases = vec![
        WriteCase {
            case: "read_only_inside",
            target: workspace.join("readonly.txt"),
            writable: false,
            expected: false,
        },
        WriteCase {
            case: "read_only_outside",
            target: outside.join("readonly.txt"),
            writable: false,
            expected: false,
        },
        WriteCase {
            case: "workspace_write_inside",
            target: workspace.join("allowed.txt"),
            writable: true,
            expected: true,
        },
        WriteCase {
            case: "workspace_write_sibling",
            target: outside.join("outside.txt"),
            writable: true,
            expected: false,
        },
        WriteCase {
            case: "workspace_write_symlink",
            target: workspace.join("external-link").join("linked.txt"),
            writable: true,
            expected: false,
        },
        WriteCase {
            case: "workspace_write_git",
            target: workspace.join(".git").join("config"),
            writable: true,
            expected: false,
        },
        WriteCase {
            case: "workspace_write_codex",
            target: workspace.join(".codex").join("config.toml"),
            writable: true,
            expected: false,
        },
        WriteCase {
            case: "workspace_write_agents",
            target: workspace.join(".agents").join("probe.txt"),
            writable: true,
            expected: false,
        },
        WriteCase {
            case: "workspace_write_tmpdir",
            target: root.path().join("tmp").join("probe.txt"),
            writable: true,
            expected: false,
        },
        WriteCase {
            case: "workspace_write_slash_tmp",
            target: slash_tmp.path().join("probe.txt"),
            writable: true,
            expected: false,
        },
    ];
    for case in &cases {
        std::fs::write(&case.target, SANDBOX_ORIGINAL).ok()?;
    }
    // The network cases need a real interpreter and a loopback listener; the
    // probe keeps 10 cases when there is none instead of guessing.
    let python = find_python(env);
    let listener = python
        .as_ref()
        .and_then(|_| TcpListener::bind(("127.0.0.1", 0)).ok());
    Some(SandboxPlan {
        workspace,
        cases,
        python: listener.as_ref().and(python),
        listener,
        _root: root,
        _slash_tmp: slash_tmp,
    })
}

#[cfg(unix)]
fn link_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn link_dir(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("symlinks are not probed on this OS"))
}

fn find_python(env: &DetectionEnv) -> Option<String> {
    let path = env.path.as_deref()?;
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("python3"))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| candidate.to_str().map(str::to_owned))
}

fn sandbox_policy(workspace: &Path, writable: bool) -> Value {
    if !writable {
        return json!({ "type": "readOnly", "networkAccess": false });
    }
    json!({
        "type": "workspaceWrite",
        "writableRoots": [workspace.to_string_lossy()],
        "networkAccess": false,
        "excludeSlashTmp": true,
        "excludeTmpdirEnvVar": true,
    })
}

/// Every case, or `None`. An inconclusive run (transport error, budget spent,
/// a server without `command/exec`) must not look like a boundary failure:
/// `local_probe_failed` is permanent, "not measured" is not.
fn codex_sandbox_cases(
    peer: &dyn ProtocolPeer,
    next_id: &mut u64,
    plan: &SandboxPlan,
    deadline: Instant,
) -> Option<Vec<(&'static str, bool)>> {
    let mut results: Vec<(&'static str, bool)> = Vec::new();
    for case in &plan.cases {
        if remaining(deadline).is_zero() {
            return None;
        }
        // `sh -c 'printf mutated > "$1"' sh <path>`: no interpreter needed,
        // and the path stays an argument instead of shell text.
        let command = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "printf mutated > \"$1\"".to_owned(),
            "sh".to_owned(),
            case.target.to_string_lossy().into_owned(),
        ];
        let (exit, _, _) = exec_case(
            peer,
            next_id,
            command,
            &plan.workspace,
            sandbox_policy(&plan.workspace, case.writable),
            deadline,
        )?;
        let changed = std::fs::read(&case.target)
            .map(|bytes| bytes != SANDBOX_ORIGINAL)
            .unwrap_or(true);
        let passed = changed == case.expected && (!case.expected || exit == 0);
        results.push((case.case, passed));
    }
    if let (Some(python), Some(listener)) = (plan.python.as_deref(), plan.listener.as_ref()) {
        let port = listener.local_addr().ok()?.port();
        for (case, writable) in [
            ("read_only_network", false),
            ("workspace_write_network", true),
        ] {
            if remaining(deadline).is_zero() {
                return None;
            }
            let command = vec![
                python.to_owned(),
                "-c".to_owned(),
                "import socket,sys; s=socket.create_connection(('127.0.0.1',int(sys.argv[1])),timeout=2); s.close()".to_owned(),
                port.to_string(),
            ];
            let (exit, _, stderr) = exec_case(
                peer,
                next_id,
                command,
                &plan.workspace,
                sandbox_policy(&plan.workspace, writable),
                deadline,
            )?;
            // The libc message is localized; Python's exception class name is
            // not, so either spelling counts as the sandbox refusing the connect.
            let denied =
                stderr.contains("Operation not permitted") || stderr.contains("PermissionError");
            results.push((case, exit == 1 && denied));
        }
    }
    Some(results)
}

/// One buffered `command/exec`. `None` means the case could not be observed.
fn exec_case(
    peer: &dyn ProtocolPeer,
    next_id: &mut u64,
    command: Vec<String>,
    cwd: &Path,
    policy: Value,
    deadline: Instant,
) -> Option<(i64, String, String)> {
    *next_id += 1;
    let response = request(
        peer,
        *next_id,
        "command/exec",
        json!({
            "command": command,
            "cwd": cwd.to_string_lossy(),
            "sandboxPolicy": policy,
            "timeoutMs": step_timeout(deadline).as_millis() as u64,
        }),
    )
    .ok()?;
    let result = response.get("result")?;
    Some((
        result.get("exitCode").and_then(Value::as_i64)?,
        result
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        result
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    ))
}

/// Send a request and wait for the response carrying `id`; anything else on
/// the wire is ignored (this session starts no thread, so nothing inbound
/// needs an answer). The caller's deadline ends a silent server.
fn request(peer: &dyn ProtocolPeer, id: u64, method: &str, params: Value) -> Result<Value, ()> {
    peer.send(&json!({ "id": id, "method": method, "params": params }))
        .map_err(|_| ())?;
    loop {
        match peer.recv() {
            PeerEvent::Message(value) => {
                let addressed = match value.get("id") {
                    Some(Value::Number(number)) => {
                        number.as_u64() == Some(id) || number.as_i64() == Some(id as i64)
                    }
                    _ => false,
                };
                if addressed && (value.get("result").is_some() || value.get("error").is_some()) {
                    return Ok(value);
                }
            }
            PeerEvent::Eof | PeerEvent::ConnectionLost | PeerEvent::Overcap => return Err(()),
        }
    }
}

/// Send a notification (no id, no response).
fn notify(peer: &dyn ProtocolPeer, method: &str, params: Value) -> Result<(), ()> {
    peer.send(&json!({ "method": method, "params": params }))
        .map_err(|_| ())
}

// ---- OpenCode: owned metadata-only server ------------------------------------

/// Pure report builder for the OpenCode row.
pub fn opencode_report(
    health_ok: bool,
    missing_routes: &[&str],
    model_listed: Option<bool>,
) -> LocalProbeReport {
    LocalProbeReport {
        protocol_ok: health_ok && missing_routes.is_empty(),
        model_listed,
        failures: missing_routes
            .iter()
            .map(|route| format!("{ROUTE_MISSING}{route}"))
            .collect(),
        ..empty_report()
    }
}

fn opencode_probe(
    binding: &Binding,
    program: &str,
    version: &str,
    env: &DetectionEnv,
    deadline: Instant,
) -> LocalProbeReport {
    // The adapter cannot launch without a saved connection, so there is
    // nothing to measure — and the probe never resolves the secret itself.
    if binding.credential_ref.is_none() || binding.endpoint_ref.is_none() {
        return failed_report(FAIL_CONNECTION_REQUIRED);
    }
    let Some(server) = OwnedServer::start(program, env, deadline) else {
        return failed_report(FAIL_SERVER_UNAVAILABLE);
    };
    // `HttpTransport` has a fixed per-request ceiling well above the probe
    // budget, so the metadata reads run on their own thread and this one
    // enforces the deadline (11 §6). On a timeout the worker is left to fail
    // on its own: dropping the server below kills the process, so every
    // pending request errors out and the thread unwinds.
    let (report, results) = mpsc::channel();
    let address = server.address;
    let password = server.password.clone();
    let target = binding.clone();
    let version = version.to_owned();
    let spawned = std::thread::Builder::new()
        .name("opencode-metadata-probe".into())
        .spawn(move || {
            let _ = report.send(opencode_metadata(
                address, &password, &target, &version, deadline,
            ));
        });
    let report = match spawned {
        Ok(_) => results
            .recv_timeout(remaining(deadline))
            .unwrap_or_else(|_| failed_report(FAIL_SERVER_UNAVAILABLE)),
        Err(_) => failed_report(FAIL_SERVER_UNAVAILABLE),
    };
    // `OwnedServer::drop` kills the process group and reaps it; do it now so
    // the caller never returns before the child is gone.
    drop(server);
    report
}

fn opencode_metadata(
    address: SocketAddr,
    password: &str,
    binding: &Binding,
    version: &str,
    deadline: Instant,
) -> LocalProbeReport {
    use super::opencode::OpencodeTransport;
    let Ok(transport) = super::opencode::http::HttpTransport::new(
        address,
        password,
        Arc::new(|| {}),
        Arc::new(|| super::RunProbe::Unknown),
    ) else {
        return failed_report(FAIL_SERVER_UNAVAILABLE);
    };
    // Each read is checked against the budget before it starts; a read that
    // is still in flight when the budget ends is cut by the caller.
    if remaining(deadline).is_zero() || transport.verify_authentication().is_err() {
        return failed_report(FAIL_SERVER_UNAVAILABLE);
    }
    if remaining(deadline).is_zero() {
        return failed_report(FAIL_SERVER_UNAVAILABLE);
    }
    let Ok(health) = transport.get("/global/health") else {
        return failed_report(FAIL_SERVER_UNAVAILABLE);
    };
    if health["healthy"] != true {
        return failed_report(FAIL_SERVER_UNAVAILABLE);
    }
    if health["version"].as_str() != Some(version) {
        return failed_report(FAIL_VERSION_CHANGED);
    }
    if remaining(deadline).is_zero() {
        return failed_report(FAIL_SERVER_UNAVAILABLE);
    }
    let Ok(document) = transport.get("/doc") else {
        return failed_report(FAIL_SERVER_UNAVAILABLE);
    };
    let missing: Vec<&str> = OPENCODE_ROUTES
        .iter()
        .filter(|(route, _)| document["paths"].get(route).is_none())
        .map(|(_, slug)| *slug)
        .collect();
    // Out of budget here is not a failure of the server: the routes above
    // already settled `protocol_ok`, only the listing hint is skipped.
    let model_listed = (!remaining(deadline).is_zero())
        .then(|| transport.get("/config/providers").ok())
        .flatten()
        .map(|providers| lists_model(&providers, &binding.provider_id, &binding.model_id));
    opencode_report(true, &missing, model_listed)
}

/// `/config/providers` shape: `{ "providers": [{ "id": ..., "models": { id: {…} } }] }`.
/// Only ids are read; nothing else from the catalog is kept.
fn lists_model(providers: &Value, provider_id: &str, model_id: &str) -> bool {
    let Some(entries) = providers["providers"].as_array() else {
        return false;
    };
    entries.iter().any(|entry| {
        entry["id"].as_str() == Some(provider_id)
            && match &entry["models"] {
                Value::Object(models) => models.contains_key(model_id),
                Value::Array(models) => models
                    .iter()
                    .any(|model| model["id"].as_str() == Some(model_id)),
                _ => false,
            }
    })
}

/// A probe-owned OpenCode server: isolated configuration directories, a
/// generated Basic-auth password, no inherited provider credentials, and a
/// guaranteed reap.
struct OwnedServer {
    child: Child,
    reaped: bool,
    address: SocketAddr,
    password: String,
    _private: tempfile::TempDir,
}

impl OwnedServer {
    fn start(program: &str, env: &DetectionEnv, deadline: Instant) -> Option<Self> {
        let private = tempfile::Builder::new()
            .prefix("iyagi-opencode-probe-")
            .tempdir()
            .ok()?;
        let workspace = private.path().join("workspace");
        std::fs::create_dir_all(&workspace).ok()?;
        let password = format!(
            "{}{}",
            term_contracts::mission::types::Id::generate(),
            term_contracts::mission::types::Id::generate()
        );
        let mut command = Command::new(program);
        command
            .args(OPENCODE_SERVE_ARGV)
            .current_dir(&workspace)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // Diagnostics are never read, so a full pipe can never stall the
            // server and no CLI text can reach a report.
            .stderr(Stdio::null());
        if let Some(path) = env.path.as_deref() {
            command.env("PATH", path);
        }
        for name in [
            "LANG",
            "LC_ALL",
            "TZ",
            "SystemRoot",
            "WINDIR",
            "COMSPEC",
            "PATHEXT",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        for (name, sub) in [
            ("HOME", "home"),
            ("USERPROFILE", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
            ("OPENCODE_CONFIG_DIR", "config"),
            ("TMPDIR", "tmp"),
            ("TMP", "tmp"),
            ("TEMP", "tmp"),
        ] {
            let directory = private.path().join(sub);
            std::fs::create_dir_all(&directory).ok()?;
            command.env(name, &directory);
        }
        for name in [
            "OPENCODE_DISABLE_PROJECT_CONFIG",
            "OPENCODE_DISABLE_AUTOUPDATE",
            "OPENCODE_DISABLE_PRUNE",
            "OPENCODE_DISABLE_LSP_DOWNLOAD",
            "OPENCODE_DISABLE_CLAUDE_CODE",
            "OPENCODE_DISABLE_EXTERNAL_SKILLS",
            "OPENCODE_DISABLE_DEFAULT_PLUGINS",
        ] {
            command.env(name, "true");
        }
        command.env("OPENCODE_SERVER_USERNAME", "opencode");
        command.env("OPENCODE_SERVER_PASSWORD", &password);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command.spawn().ok()?;
        let mut server = OwnedServer {
            address: SocketAddr::from(([127, 0, 0, 1], 0)),
            password,
            reaped: false,
            _private: private,
            child,
        };
        let stdout = server.child.stdout.take()?;
        let (found, addresses) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("opencode-local-probe".into())
            .spawn(move || {
                let mut announced = false;
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else { return };
                    if announced {
                        continue; // keep draining so the server never blocks
                    }
                    if let Some(address) = listening_address(&crate::agent_model::strip_ansi(&line))
                    {
                        announced = found.send(address).is_ok();
                    }
                }
            });
        if reader.is_err() {
            return None;
        }
        server.address = addresses.recv_timeout(remaining(deadline)).ok()?;
        Some(server)
    }

    fn reap(&mut self) {
        if self.reaped {
            return;
        }
        self.reaped = true;
        #[cfg(unix)]
        {
            let pid = self.child.id() as i32;
            if pid > 1 {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for OwnedServer {
    fn drop(&mut self) {
        self.reap();
    }
}

/// The literal owned-loopback announcement (same rule as
/// `opencode::server::listening_address`): anything else is not our server.
fn listening_address(line: &str) -> Option<SocketAddr> {
    let address: SocketAddr = line
        .trim()
        .strip_prefix("opencode server listening on http://")?
        .parse()
        .ok()?;
    (address.ip() == std::net::Ipv4Addr::LOCALHOST && address.port() != 0).then_some(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::claude::FIXTURE_HELP;

    fn codex_caps(report: &LocalProbeReport) -> RuntimeCapabilities {
        capabilities(RuntimeKind::Codex, report)
    }

    #[test]
    fn claude_help_proves_every_flag_the_adapter_passes() {
        let report = claude_help_report(FIXTURE_HELP);
        assert!(report.protocol_ok, "{:?}", report.failures);
        assert!(report.failures.is_empty());
        assert_eq!(report.model_listed, None);
        assert_eq!(report.sandbox_cases_total, None);
        let caps = capabilities(RuntimeKind::Claude, &report);
        for support in [
            &caps.structured_result,
            &caps.events,
            &caps.cancel,
            &caps.usage,
            &caps.read_only,
            &caps.scoped_write,
        ] {
            assert_eq!(support, &proven());
        }
        // The probe never invents support the adapter does not implement.
        assert!(!caps.steer.supported && !caps.approval_reply.supported && !caps.resume.supported);
    }

    #[test]
    fn a_missing_claude_flag_disproves_exactly_what_depends_on_it() {
        let stripped = FIXTURE_HELP.replace("--json-schema", "--json-schema-removed");
        let report = claude_help_report(&stripped);
        assert!(!report.protocol_ok);
        assert_eq!(report.failures, vec!["flag_missing:--json-schema"]);
        let caps = capabilities(RuntimeKind::Claude, &report);
        assert_eq!(caps.structured_result, disproven());
        assert_eq!(
            caps.events,
            proven(),
            "another flag's absence is not this one's"
        );
        assert_eq!(caps.read_only, proven());

        let no_permissions = FIXTURE_HELP.replace("--permission-mode", "--permission-mode-removed");
        let caps = capabilities(RuntimeKind::Claude, &claude_help_report(&no_permissions));
        assert_eq!(caps.read_only, disproven());
        assert_eq!(caps.scoped_write, disproven());
        assert_eq!(caps.structured_result, proven());

        // An unreadable help text proves nothing at all — it must not look
        // like a disproof, which consent could never recover from.
        let unread = claude_help_report("   ");
        assert_eq!(unread.failures, vec![FAIL_HELP_UNAVAILABLE]);
        assert_eq!(capabilities(RuntimeKind::Claude, &unread), unclaimed());
    }

    #[test]
    fn flag_matching_is_whole_token() {
        assert!(advertises("  --output-format <format>", "--output-format"));
        assert!(advertises(
            "use --output-format=stream-json here",
            "--output-format"
        ));
        assert!(advertises(
            "  --allowedTools, --allowed-tools <tools...>",
            "--allowedTools"
        ));
        assert!(!advertises(
            "  --allowed-tools <tools...>",
            "--allowedTools"
        ));
        assert!(!advertises("  --model-set-by <x>", "--model"));
        assert!(advertises("  --model <model>", "--model"));
    }

    #[test]
    fn codex_handshake_and_sandbox_cases_map_to_their_own_capabilities() {
        // Handshake only (runtime.detect): protocol capabilities, no
        // workspace claim, and no model listing when there was none.
        let cheap = codex_report(true, None, None);
        let caps = codex_caps(&cheap);
        for support in [
            &caps.structured_result,
            &caps.events,
            &caps.cancel,
            &caps.steer,
            &caps.approval_reply,
            &caps.usage,
        ] {
            assert_eq!(support, &proven());
        }
        assert!(!caps.model_listing.supported);
        assert!(!caps.read_only.supported && !caps.scoped_write.supported);
        assert_eq!(
            caps.read_only.reason_code.as_deref(),
            Some("no_compatibility_evidence")
        );

        // A listing that was read proves the listing capability even when the
        // bound model is not in it (the UI warns; the gate does not block).
        let listed = codex_report(true, Some(false), None);
        assert_eq!(codex_caps(&listed).model_listing, proven());

        // Every boundary case held: the workspace claims are earned.
        let all = [("read_only_inside", true), ("workspace_write_inside", true)];
        let full = codex_report(true, Some(true), Some(&all));
        assert_eq!(full.sandbox_cases_passed, Some(2));
        assert_eq!(full.sandbox_cases_total, Some(2));
        assert!(full.failures.is_empty());
        let caps = codex_caps(&full);
        assert_eq!(caps.read_only, proven());
        assert_eq!(caps.scoped_write, proven());

        // One failure is enough to disprove both, permanently.
        let partial = [
            ("read_only_inside", true),
            ("workspace_write_sibling", false),
        ];
        let broken = codex_report(true, Some(true), Some(&partial));
        assert_eq!(broken.failures, vec!["sandbox:workspace_write_sibling"]);
        assert_eq!(broken.sandbox_cases_passed, Some(1));
        let caps = codex_caps(&broken);
        assert_eq!(caps.read_only, disproven());
        assert_eq!(caps.scoped_write, disproven());
        assert_eq!(caps.events, proven(), "the protocol still handshook");

        // No handshake: nothing is claimed and nothing is disproven, so
        // consent stays possible for an adapter-implemented capability.
        let none = codex_report(false, None, None);
        assert_eq!(none.failures, vec![FAIL_HANDSHAKE]);
        assert_eq!(codex_caps(&none), unclaimed());
    }

    #[test]
    fn opencode_needs_health_and_every_adapter_route() {
        let healthy = opencode_report(true, &[], Some(true));
        assert!(healthy.protocol_ok);
        let caps = capabilities(RuntimeKind::Opencode, &healthy);
        for support in [
            &caps.structured_result,
            &caps.events,
            &caps.cancel,
            &caps.approval_reply,
            &caps.usage,
            &caps.read_only,
            &caps.scoped_write,
            &caps.model_listing,
        ] {
            assert_eq!(support, &proven());
        }
        assert!(!caps.steer.supported && !caps.resume.supported);
        let missing = opencode_report(true, &["session_prompt"], None);
        assert!(!missing.protocol_ok);
        assert_eq!(missing.failures, vec!["route_missing:session_prompt"]);
        assert_eq!(capabilities(RuntimeKind::Opencode, &missing), unclaimed());
        assert_eq!(
            capabilities(RuntimeKind::Fake, &opencode_report(true, &[], Some(true))),
            unclaimed()
        );
    }

    #[test]
    fn opencode_readiness_accepts_only_the_owned_loopback_announcement() {
        assert!(
            listening_address("opencode server listening on http://127.0.0.1:34567\n").is_some()
        );
        for line in [
            "http://127.0.0.1:34567",
            "opencode server listening on http://0.0.0.0:34567",
            "opencode server listening on http://127.0.0.1:0",
            "opencode server listening on http://127.0.0.1:34567/path",
        ] {
            assert!(listening_address(line).is_none(), "{line}");
        }
    }

    #[test]
    fn provider_listings_are_matched_by_id_only() {
        let providers = json!({
            "providers": [
                {"id": "zai-coding-plan", "models": {"glm-5.3": {"name": "GLM"}}},
                {"id": "other", "models": {"m": {}}}
            ]
        });
        assert!(lists_model(&providers, "zai-coding-plan", "glm-5.3"));
        assert!(!lists_model(&providers, "zai-coding-plan", "glm-9"));
        assert!(!lists_model(&providers, "openai", "glm-5.3"));
        let array_shape = json!({"providers": [{"id": "p", "models": [{"id": "m"}]}]});
        assert!(lists_model(&array_shape, "p", "m"));
        assert!(!lists_model(&json!({}), "p", "m"));
    }

    #[test]
    fn model_advertisement_reads_both_id_fields_and_skips_hidden() {
        assert!(advertises_model(
            &json!({"model": "gpt-5.6-luna"}),
            "gpt-5.6-luna"
        ));
        assert!(advertises_model(
            &json!({"id": "gpt-5.6-luna"}),
            "gpt-5.6-luna"
        ));
        assert!(!advertises_model(
            &json!({"model": "gpt-5.6-luna", "hidden": true}),
            "gpt-5.6-luna"
        ));
        assert!(!advertises_model(
            &json!({"model": "other"}),
            "gpt-5.6-luna"
        ));
    }

    #[test]
    fn the_fake_runtime_is_never_probed() {
        let binding = crate::agent_runtime::fake::fake_binding();
        let report = run(
            &binding,
            "term-fixture",
            "fixture-v1",
            &DetectionEnv::default(),
            Duration::from_millis(1),
        );
        assert_eq!(report, empty_report());
        assert_eq!(
            run_cheap(
                RuntimeKind::Opencode,
                "opencode",
                "m",
                &DetectionEnv::default(),
                Duration::from_millis(1)
            ),
            None
        );
    }

    /// Live checks against an installed CLI. Explicitly opted into with the
    /// executable path, never discovered from PATH inside a test run.
    #[test]
    #[ignore = "requires IYAGI_CODEX_PROBE_PROGRAM; spawns the installed app-server"]
    fn installed_codex_completes_the_handshake_and_sandbox_cases() {
        let program = std::env::var("IYAGI_CODEX_PROBE_PROGRAM").expect("installed CLI path");
        let model = std::env::var("IYAGI_CODEX_PROBE_MODEL").unwrap_or_default();
        let report = codex_probe(
            &program,
            &model,
            &DetectionEnv::current(),
            Instant::now() + BUDGET,
            true,
        );
        assert!(report.protocol_ok, "{:?}", report.failures);
        assert_eq!(
            report.sandbox_cases_passed, report.sandbox_cases_total,
            "{:?}",
            report.failures
        );
    }

    #[test]
    #[ignore = "requires IYAGI_CLAUDE_PROBE_PROGRAM; runs `claude --help`"]
    fn installed_claude_advertises_every_flag() {
        let program = std::env::var("IYAGI_CLAUDE_PROBE_PROGRAM").expect("installed CLI path");
        let report = claude_probe(&program, Instant::now() + BUDGET);
        assert!(report.protocol_ok, "{:?}", report.failures);
    }
}
