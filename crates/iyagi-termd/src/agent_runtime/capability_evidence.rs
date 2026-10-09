//! Compatibility evidence registry (03 §6, tickets O18 and O22).
//!
//! Shipped entries record the exact runtime+version+OS+auth-route combination
//! a real session proved, plus the fixture digest of the recorded transcript.
//! 11 §2 adds three more layers measured on the user's machine, so a claim is
//! no longer limited to the versions the author happened to record:
//!
//! 1. shipped evidence for this exact version — reason `null`;
//! 2. the same OS/major line of that shipped evidence, but only while this
//!    version's own protocol self-check passed — reason `version_line`;
//! 3. the daemon's no-inference self-check on this machine (`local_probe.rs`)
//!    — reason `local_probe`, and `local_probe_failed` when it proved the
//!    opposite (consent cannot re-open those);
//! 4. successful Run observations for this machine/version/model — reason
//!    `observed_runs`;
//! 5. the user's per-connection experimental consent — `experimental_opt_in`,
//!    bounded by what the adapter actually implements.
//!
//! Role gates still look at `supported` only; the reason codes are display
//! and precedence metadata. Evidence never crosses an OS boundary, and local
//! evidence applies only while its `os`/`version` equal the observation.

use term_contracts::mission::rpc::CompatibilityGrade;
use term_contracts::mission::types::{
    AuthRoute, Binding, LocalEvidence, LocalRunEvidence, RuntimeCapabilities, RuntimeKind, Support,
};

/// The shared "not yet proven live" reason slug.
pub const NO_COMPATIBILITY_EVIDENCE: &str = "no_compatibility_evidence";
/// Support granted only by the user's explicit opt-in for this connection.
pub const EXPERIMENTAL_OPT_IN: &str = "experimental_opt_in";
/// The production adapter has no code path for this capability; consent
/// cannot enable it.
pub const ADAPTER_NOT_IMPLEMENTED: &str = "adapter_not_implemented";
/// Shipped evidence of the same OS/major line applied to a newer version
/// whose local protocol self-check passed (11 §3.1).
pub const VERSION_LINE: &str = "version_line";
/// The daemon's no-inference self-check proved this capability here.
pub const LOCAL_PROBE: &str = "local_probe";
/// The self-check ran and this capability failed — consent cannot open it.
pub const LOCAL_PROBE_FAILED: &str = "local_probe_failed";
/// Successful Run observations on this machine/version/model proved it.
pub const OBSERVED_RUNS: &str = "observed_runs";

/// Canonical daemon-side OS slug (std::env::consts::OS).
pub const OS_WINDOWS: &str = "windows";
pub const OS_LINUX: &str = "linux";
pub const OS_MACOS: &str = "macos";

fn yes() -> Support {
    Support {
        supported: true,
        reason_code: None,
    }
}

fn no() -> Support {
    Support {
        supported: false,
        reason_code: Some(NO_COMPATIBILITY_EVIDENCE.into()),
    }
}

fn no_reason(reason: &'static str) -> Support {
    Support {
        supported: false,
        reason_code: Some(reason.into()),
    }
}

/// Supported, with the evidence layer that granted it (11 §3.1).
fn yes_reason(reason: &'static str) -> Support {
    Support {
        supported: true,
        reason_code: Some(reason.into()),
    }
}

pub fn unclaimed() -> RuntimeCapabilities {
    let no = no();
    RuntimeCapabilities {
        structured_result: no.clone(),
        events: no.clone(),
        cancel: no.clone(),
        resume: no.clone(),
        steer: no.clone(),
        approval_reply: no.clone(),
        read_only: no.clone(),
        scoped_write: no.clone(),
        model_listing: no.clone(),
        usage: no.clone(),
        native_terminal_attach: no,
    }
}

/// Provider of the subscription route that recorded live evidence for a
/// runtime. `runtime.detect` suggests exactly this value.
pub fn evidence_provider_id(runtime: RuntimeKind) -> Option<&'static str> {
    match runtime {
        RuntimeKind::Codex => Some("openai"),
        RuntimeKind::Claude => Some("anthropic"),
        RuntimeKind::Opencode => Some("zai-coding-plan"),
        RuntimeKind::Fake => None,
    }
}

/// The one model this OS's evidence was recorded with, when evidence is
/// model-pinned. Any other model stays unclaimed on that OS.
pub fn evidence_model_id(runtime: RuntimeKind, os: &str) -> Option<&'static str> {
    (runtime == RuntimeKind::Codex && os == OS_MACOS).then_some(CODEX_MACOS_MODEL)
}

/// Layered projection of 11 §4: for every capability the first layer that
/// says `supported` wins, in the order shipped → version line → local probe →
/// observed runs → experimental consent. A capability the local self-check
/// disproved is never re-opened by consent.
///
/// Shipped evidence must still match the provider, authentication route and
/// (where the fixture is model-pinned) the model — stored-key/token routes and
/// custom endpoints inherit neither the exact fixture nor its version line.
pub fn capabilities_for_binding(
    binding: &Binding,
    os: &str,
    version: Option<&str>,
) -> RuntimeCapabilities {
    let shipped = evidence_for_binding(binding, os, version);
    let local = current_local_evidence(binding, os, version);
    let probe_report = local.and_then(|local| local.probe.as_ref());
    // (2) The same line, but only once this version's own protocol
    // self-check passed. Without it the binding stays `SameLineUnverified`.
    let line = (!claims_any(&shipped)
        && probe_report.is_some_and(|report| report.protocol_ok)
        && shipped_route_matches(binding))
    .then(|| line_evidence_version(binding.runtime, os, version))
    .flatten()
    .map(|evidence| capabilities_for(binding.runtime, os, Some(evidence)));
    // (3) This machine's own self-check, (4) its successful runs — run
    // counters belong to the model they were collected for.
    let probe =
        probe_report.map(|report| super::local_probe::capabilities(binding.runtime, report));
    let runs = local
        .filter(|local| local.model_id == binding.model_id)
        .map(|local| run_capabilities(&local.runs));
    // (5) Consent, bounded by what the adapter implements.
    let experimental = experimental_applies(binding).then(|| adapter_capabilities(binding.runtime));
    macro_rules! layered {
        ($field:ident) => {
            pick(
                &shipped.$field,
                line.as_ref().map(|caps| &caps.$field),
                probe.as_ref().map(|caps| &caps.$field),
                runs.as_ref().map(|caps| &caps.$field),
                experimental.as_ref().map(|caps| &caps.$field),
            )
        };
    }
    RuntimeCapabilities {
        structured_result: layered!(structured_result),
        events: layered!(events),
        cancel: layered!(cancel),
        resume: layered!(resume),
        steer: layered!(steer),
        approval_reply: layered!(approval_reply),
        read_only: layered!(read_only),
        scoped_write: layered!(scoped_write),
        model_listing: layered!(model_listing),
        usage: layered!(usage),
        native_terminal_attach: layered!(native_terminal_attach),
    }
}

/// One capability across the five layers (11 §4). Absent layers are `None`,
/// which is not the same as a layer that looked and found nothing.
fn pick(
    shipped: &Support,
    line: Option<&Support>,
    probe: Option<&Support>,
    runs: Option<&Support>,
    experimental: Option<&Support>,
) -> Support {
    let probe_failed = probe.is_some_and(|support| {
        !support.supported && support.reason_code.as_deref() == Some(LOCAL_PROBE_FAILED)
    });
    if shipped.supported {
        return yes();
    }
    if line.is_some_and(|support| support.supported) {
        return yes_reason(VERSION_LINE);
    }
    if probe.is_some_and(|support| support.supported) {
        return yes_reason(LOCAL_PROBE);
    }
    if runs.is_some_and(|support| support.supported) {
        return yes_reason(OBSERVED_RUNS);
    }
    if !probe_failed && experimental.is_some_and(|support| support.supported) {
        return yes_reason(EXPERIMENTAL_OPT_IN);
    }
    // Nothing granted it: the most specific "why not" wins.
    if probe_failed {
        return no_reason(LOCAL_PROBE_FAILED);
    }
    if experimental.is_some() {
        return no_reason(ADAPTER_NOT_IMPLEMENTED);
    }
    shipped.clone()
}

/// Stored local evidence that still describes the current installation.
/// A different OS or CLI version makes it inapplicable, never wrong.
fn current_local_evidence<'a>(
    binding: &'a Binding,
    os: &str,
    version: Option<&str>,
) -> Option<&'a LocalEvidence> {
    let local = binding.local_evidence.as_ref()?;
    (local.os == os && version.is_some_and(|version| version == local.version)).then_some(local)
}

/// 11 §4 step 4. Counters are evidence only while structured results kept
/// arriving: more invalid results than successes promotes nothing at all.
fn run_capabilities(runs: &LocalRunEvidence) -> RuntimeCapabilities {
    let mut capabilities = unclaimed();
    let total = runs
        .succeeded_read_only
        .saturating_add(runs.succeeded_write);
    if runs.invalid_result > total {
        return capabilities;
    }
    if total >= 3 {
        capabilities.structured_result = yes();
        capabilities.events = yes();
        capabilities.usage = yes();
        if runs.succeeded_read_only >= 1 {
            capabilities.read_only = yes();
        }
        if runs.succeeded_write >= 1 {
            capabilities.scoped_write = yes();
        }
    }
    if runs.cancelled >= 1 {
        capabilities.cancel = yes();
    }
    capabilities
}

/// Shipped evidence is keyed by provider + authentication route: a
/// stored-key/token route or a custom endpoint inherits neither the exact
/// fixture nor its version line, because nobody ran that route live.
fn shipped_route_matches(binding: &Binding) -> bool {
    let references_match = match binding.runtime {
        RuntimeKind::Codex | RuntimeKind::Claude => {
            binding.credential_ref.is_none() && binding.endpoint_ref.is_none()
        }
        RuntimeKind::Opencode => binding.credential_ref.is_some() && binding.endpoint_ref.is_some(),
        RuntimeKind::Fake => false,
    };
    references_match
        && binding.auth_route == AuthRoute::Subscription
        && evidence_provider_id(binding.runtime) == Some(binding.provider_id.as_str())
}

/// The exact fixture is additionally pinned to the model it was recorded with
/// where one exists. The version line deliberately drops this pin (11 §8:
/// model suitability is the runtime's `RESULT_INVALID` + plan repair, not a
/// capability gate), so a different model needs a probe, never consent.
fn shipped_model_matches(binding: &Binding, os: &str) -> bool {
    !evidence_model_id(binding.runtime, os).is_some_and(|model| binding.model_id != model)
}

/// Shipped-evidence-only projection (no line, local or consent layer). The
/// `runtime.detect` grade is decided on this alone.
pub fn evidence_for_binding(
    binding: &Binding,
    os: &str,
    version: Option<&str>,
) -> RuntimeCapabilities {
    if !shipped_route_matches(binding) || !shipped_model_matches(binding, os) {
        return unclaimed();
    }
    capabilities_for(binding.runtime, os, version)
}

/// Consent is per connection, not per CLI version (11 §3.4): an updated CLI
/// re-runs the self-check instead of asking again. A route the adapter cannot
/// launch, and the fake runtime, still ignore it.
pub fn experimental_applies(binding: &Binding) -> bool {
    binding.runtime != RuntimeKind::Fake
        && binding
            .experimental_version
            .as_deref()
            .is_some_and(|accepted| !accepted.trim().is_empty())
        && adapter_supports_route(binding)
}

/// Provider/auth routes each production adapter resolves at launch
/// (codex/auth.rs, claude/auth.rs, opencode runtime + connections.rs).
pub fn adapter_supports_route(binding: &Binding) -> bool {
    let refs = (
        binding.credential_ref.is_some(),
        binding.endpoint_ref.is_some(),
    );
    match binding.runtime {
        RuntimeKind::Codex => {
            binding.provider_id == "openai"
                && match binding.auth_route {
                    AuthRoute::Subscription => refs == (false, false),
                    AuthRoute::ApiKey => refs == (true, true),
                    AuthRoute::Local | AuthRoute::Custom => false,
                }
        }
        RuntimeKind::Claude => matches!(
            (binding.provider_id.as_str(), binding.auth_route, refs),
            ("anthropic", AuthRoute::Subscription, (false, false))
                // Z.ai Coding Plan also launches on the daemon's own key
                // store — the same credential `claude-exec --provider zai`
                // (the `ccg` launch profile) hands the terminal CLI.
                | ("zai-coding-plan", AuthRoute::Subscription, (false, false))
                | (
                    "anthropic" | "zai-coding-plan",
                    AuthRoute::ApiKey | AuthRoute::Subscription,
                    (true, true)
                )
        ),
        RuntimeKind::Opencode => {
            refs == (true, true)
                && matches!(
                    binding.auth_route,
                    AuthRoute::ApiKey | AuthRoute::Subscription
                )
                && !binding.provider_id.trim().is_empty()
        }
        RuntimeKind::Fake => false,
    }
}

/// What the production mission adapter code implements (03 §6 table). This
/// is not compatibility evidence; it bounds what consent may enable.
pub fn adapter_capabilities(runtime: RuntimeKind) -> RuntimeCapabilities {
    let missing = || no_reason(ADAPTER_NOT_IMPLEMENTED);
    match runtime {
        // app-server: outputSchema, notifications, turn/interrupt, turn/steer,
        // approval replies, read-only/workspace-write sandbox, model/list, usage.
        RuntimeKind::Codex => RuntimeCapabilities {
            structured_result: yes(),
            events: yes(),
            cancel: yes(),
            resume: missing(),
            steer: yes(),
            approval_reply: yes(),
            read_only: yes(),
            scoped_write: yes(),
            model_listing: yes(),
            usage: yes(),
            native_terminal_attach: missing(),
        },
        // print adapter: --json-schema, stream-json, supervised stop ladder,
        // plan/Read-Glob-Grep vs acceptEdits/Edit-Write with prompts denied,
        // result usage. Authenticated runs reject resume; no steer/approval.
        RuntimeKind::Claude => RuntimeCapabilities {
            structured_result: yes(),
            events: yes(),
            cancel: yes(),
            resume: missing(),
            steer: missing(),
            approval_reply: missing(),
            read_only: yes(),
            scoped_write: yes(),
            model_listing: missing(),
            usage: yes(),
            native_terminal_attach: missing(),
        },
        // server: json_schema format, SSE, abort + owned process close,
        // permission rules (edit only for writers, external_directory denied),
        // permission replies, message token/cost usage.
        RuntimeKind::Opencode => RuntimeCapabilities {
            structured_result: yes(),
            events: yes(),
            cancel: yes(),
            resume: missing(),
            steer: missing(),
            approval_reply: yes(),
            read_only: yes(),
            scoped_write: yes(),
            model_listing: missing(),
            usage: yes(),
            native_terminal_attach: missing(),
        },
        RuntimeKind::Fake => unclaimed(),
    }
}

/// Exact versions with recorded live evidence for this runtime on this OS.
pub fn evidence_versions(runtime: RuntimeKind, os: &str) -> Vec<&'static str> {
    match (runtime, os) {
        (RuntimeKind::Codex, OS_MACOS) if cfg!(target_arch = "aarch64") => {
            vec![CODEX_MACOS_VERSION]
        }
        (RuntimeKind::Codex, OS_WINDOWS) => vec![CODEX_WIN_VERSION],
        (RuntimeKind::Codex, OS_LINUX) => vec![CODEX_LINUX_VERSION],
        (RuntimeKind::Claude, OS_WINDOWS) => vec![CLAUDE_WIN_VERSION],
        (RuntimeKind::Claude, OS_LINUX) => vec![CLAUDE_LINUX_VERSION],
        (RuntimeKind::Opencode, OS_WINDOWS) => vec![OPENCODE_WIN_VERSION],
        (RuntimeKind::Opencode, OS_LINUX) => vec![OPENCODE_LINUX_VERSION],
        _ => Vec::new(),
    }
}

/// Versions that broke an adapter's protocol contract inside a line. An entry
/// `(runtime, since)` ends the line of every shipped evidence older than
/// `since`, so `version_line` stops applying at that CLI release (11 §5).
pub const KNOWN_BREAKS: &[(RuntimeKind, &str)] = &[];

/// `major.minor.patch`, discarding any prerelease/build suffix. A missing
/// minor or patch is zero; anything non-numeric is not a comparable version.
pub fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    fn number(part: Option<&str>) -> Option<u64> {
        match part {
            None => Some(0),
            Some(text) => text.parse::<u64>().ok(),
        }
    }
    let core = version.split(['-', '+']).next()?.trim();
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    Some((major, number(parts.next())?, number(parts.next())?))
}

/// Is `version` on the same line as the shipped evidence version `evidence`
/// (11 §5)? Same major, not older, and no [`KNOWN_BREAKS`] entry in between.
/// The line never extends backwards: an older CLI is a different program.
///
/// 0.x releases are treated as one line on purpose — Codex ships patches
/// twice a week as `0.154 → 0.155`, and the previous `major.minor` key made
/// every one of them a brand-new, evidence-free runtime.
pub fn on_line(version: &str, evidence: &str, runtime: RuntimeKind) -> bool {
    let (Some(version), Some(evidence)) = (parse_semver(version), parse_semver(evidence)) else {
        return false;
    };
    if version.0 != evidence.0 || version < evidence {
        return false;
    }
    !line_broken(runtime, evidence, version, KNOWN_BREAKS)
}

/// Pure core of the [`KNOWN_BREAKS`] check so the boundary (`ev < since <=
/// version`) stays testable while the shipped table is empty.
fn line_broken(
    runtime: RuntimeKind,
    evidence: (u64, u64, u64),
    version: (u64, u64, u64),
    breaks: &[(RuntimeKind, &str)],
) -> bool {
    breaks.iter().any(|(kind, since)| {
        *kind == runtime
            && parse_semver(since).is_some_and(|since| evidence < since && since <= version)
    })
}

/// The shipped evidence version whose line `version` is on, if any. The
/// tables hold at most one entry per (runtime, OS).
pub fn line_evidence_version(
    runtime: RuntimeKind,
    os: &str,
    version: Option<&str>,
) -> Option<&'static str> {
    let version = version?;
    evidence_versions(runtime, os)
        .into_iter()
        .find(|evidence| on_line(version, evidence, runtime))
}

/// `runtime.detect` grade (11 §7). `shipped_claims` is
/// [`claims_any`] over [`evidence_for_binding`] — the shipped-only projection
/// — and `all_roles_verified` means the four setup roles pass without consent.
/// `protocol_ok` is this version's local self-check result, `None` when it has
/// not run here.
pub fn compatibility_grade(
    runtime: RuntimeKind,
    os: &str,
    installed: bool,
    version: Option<&str>,
    shipped_claims: bool,
    all_roles_verified: bool,
    protocol_ok: Option<bool>,
) -> CompatibilityGrade {
    if !installed {
        return CompatibilityGrade::NotInstalled;
    }
    let line = line_evidence_version(runtime, os, version).is_some();
    // Roles carried by shipped evidence — exactly, or through its line once
    // the protocol self-check confirmed this version.
    if shipped_claims || (all_roles_verified && line && protocol_ok == Some(true)) {
        return CompatibilityGrade::Verified;
    }
    if all_roles_verified {
        return CompatibilityGrade::VerifiedLocally;
    }
    if line && protocol_ok != Some(true) {
        return CompatibilityGrade::SameLineUnverified;
    }
    CompatibilityGrade::Unverified
}

/// Any supported capability in an evidence-only projection.
pub fn claims_any(capabilities: &RuntimeCapabilities) -> bool {
    [
        &capabilities.structured_result,
        &capabilities.events,
        &capabilities.cancel,
        &capabilities.resume,
        &capabilities.steer,
        &capabilities.approval_reply,
        &capabilities.read_only,
        &capabilities.scoped_write,
        &capabilities.model_listing,
        &capabilities.usage,
        &capabilities.native_terminal_attach,
    ]
    .iter()
    .any(|support| support.supported)
}

fn codex_proven() -> RuntimeCapabilities {
    RuntimeCapabilities {
        structured_result: yes(),
        events: yes(),
        cancel: yes(),
        resume: yes(),
        steer: yes(),
        approval_reply: yes(),
        read_only: yes(),
        scoped_write: no_reason("live_run_used_read_only_sandbox_only"),
        model_listing: yes(),
        usage: yes(),
        native_terminal_attach: no_reason("jsonl_stdio_no_pty"),
    }
}

fn claude_proven() -> RuntimeCapabilities {
    RuntimeCapabilities {
        structured_result: yes(),
        events: yes(),
        cancel: no(),
        resume: yes(),
        steer: no_reason("print_mode_no_active_turn_steer"),
        approval_reply: no_reason("print_mode_no_approval_reply"),
        read_only: no(),
        scoped_write: no(),
        model_listing: no(),
        usage: yes(),
        native_terminal_attach: no_reason("print_mode_piped_stdio"),
    }
}

fn codex_macos_proven() -> RuntimeCapabilities {
    RuntimeCapabilities {
        structured_result: yes(),
        events: yes(),
        cancel: yes(),
        approval_reply: yes(),
        read_only: yes(),
        scoped_write: yes(),
        model_listing: yes(),
        // No resume/steer/usage/terminal-attach claim from these three runs.
        ..unclaimed()
    }
}

fn opencode_proven() -> RuntimeCapabilities {
    RuntimeCapabilities {
        structured_result: no_reason("cli_text_only_live_server_turn_pending"),
        events: no_reason("live_sse_turn_pending"),
        cancel: no_reason("live_abort_pending"),
        resume: no_reason("cli_continue_proven_only"),
        steer: no_reason("http_surface_no_steer"),
        approval_reply: no(),
        read_only: no(),
        scoped_write: no(),
        model_listing: yes(),
        usage: no_reason("live_usage_payload_pending"),
        native_terminal_attach: no_reason("server_process"),
    }
}

// ---- Windows (2026-09-13) ---------------------------------------------------
//
// Codex 0.153.4 / ChatGPT(Pro) / stdio app-server — recorded live session
// (sha256 441eddd40fc206bc51c5de821835cc94b209d40bcc9bec9384c0834e654bbb16;
// transcript not retained in-tree):
// initialize/account/model handshake, schema-constrained final answer
// (`{"kind":"report","report_text":"ok"}`), full notification stream,
// turn/interrupt → `status:"interrupted"` (3020 ms), thread/resume →
// follow-up turn completed (`"resumed"`).
pub const CODEX_WIN_VERSION: &str = "0.153.4";
pub const CODEX_WIN_FIXTURE_SHA256: &str =
    "441eddd40fc206bc51c5de821835cc94b209d40bcc9bec9384c0834e654bbb16";
pub const LIVE_TESTED_AT: &str = "2026-09-13";

// Claude Code 2.1.263 / stored credentials / print adapter —
// `claude/fixtures/live-session.windows-2.1.263.json`
// (sha256 603eac728637a28dff26b2d0c4758229183022b19bc298fcce50e48869dd473a):
// stream-json events, result envelope (subtype success + usage block),
// --resume same session id completed.
pub const CLAUDE_WIN_VERSION: &str = "2.1.263";
pub const CLAUDE_WIN_FIXTURE_SHA256: &str =
    "603eac728637a28dff26b2d0c4758229183022b19bc298fcce50e48869dd473a";

// OpenCode 1.18.26 / Z.AI Coding Plan / explicit zai-coding-plan/glm-5.3 —
// `opencode/fixtures/live-session.windows-1.18.26.json`
// (sha256 2472794045dda633bde4f33e105a67ec790e00297f8f124cc3260adb50427f34):
// real completion ("ok"), continue-last resume ("resumed").
pub const OPENCODE_WIN_VERSION: &str = "1.18.26";
pub const OPENCODE_WIN_FIXTURE_SHA256: &str =
    "2472794045dda633bde4f33e105a67ec790e00297f8f124cc3260adb50427f34";

// ---- Linux (2026-09-13, WSL2 Ubuntu 24.04 — real Linux kernel/userspace,
// Linux CLI builds, clean PATH without Windows interop) --------------------
//
// Codex 0.154.0 / ChatGPT(Pro) — recorded live session
// (sha256 43dd8c75b4c07b66809eb27d67f58c8ab874fe6edaec004b636cc3a7abed8718;
// transcript not retained in-tree):
// same flow proven as Windows — schema-constrained report completed,
// turn/interrupt → interrupted, thread/resume → completed, ChatGPT Pro
// account read, full event stream.
pub const CODEX_LINUX_VERSION: &str = "0.154.0";
pub const CODEX_LINUX_FIXTURE_SHA256: &str =
    "43dd8c75b4c07b66809eb27d67f58c8ab874fe6edaec004b636cc3a7abed8718";

// Claude Code 2.1.270 / print adapter —
// `claude/fixtures/live-session.linux-2.1.270.json`
// (sha256 8d1b779456d9067cf0bb2fba2073f4b00321d780bafc08bc663ccbf1e9d946ae):
// stream-json events + result envelope success, --resume same session id
// completed.
pub const CLAUDE_LINUX_VERSION: &str = "2.1.270";
pub const CLAUDE_LINUX_FIXTURE_SHA256: &str =
    "8d1b779456d9067cf0bb2fba2073f4b00321d780bafc08bc663ccbf1e9d946ae";

// OpenCode 1.18.30 / Z.AI Coding Plan / explicit zai-coding-plan/glm-5.3 —
// `opencode/fixtures/live-session.linux-1.18.30.json`
// (sha256 20f32926da22939cffd7ae4fb242b9e0b97a5238103a4a503074cec7d3b23d30):
// real completion ("ok"), continue-last resume ("resumed").
pub const OPENCODE_LINUX_VERSION: &str = "1.18.30";
pub const OPENCODE_LINUX_FIXTURE_SHA256: &str =
    "20f32926da22939cffd7ae4fb242b9e0b97a5238103a4a503074cec7d3b23d30";

// ---- macOS Apple Silicon (2026-09-16, Darwin 25.6.0) -----------------------
// Production authenticated adapter + native Exec: exact selected model,
// structured report, approved workspace file creation, immediate cancellation
// acknowledged and interrupted, and durable native cleanup for all three runs.
// A separate command/exec probe checks 12 sandbox boundaries without inference.
// Both sanitized reports are pinned below; this is not a whole-mission claim.
pub const CODEX_MACOS_VERSION: &str = "0.154.0";
pub const CODEX_MACOS_MODEL: &str = "gpt-5.6-luna";
pub const CODEX_MACOS_FIXTURE_SHA256: &str =
    "f38e26477653e3a6eb40eaf40968e10c83eb35c0a5849cfae68566931cdebe16";
pub const CODEX_MACOS_SANDBOX_SHA256: &str =
    "5ef4840bb4f1431068fc2b856a0801c1a23d5a6067fe543418d8463b38803b3c";

/// Select the evidence-matched capability set for a runtime on a given OS
/// (`std::env::consts::OS`), falling back to fully-unclaimed when the
/// (OS, version) pair does not match any recorded live run. Evidence never
/// crosses OS or version boundaries (03 §6).
pub fn capabilities_for(
    runtime: RuntimeKind,
    os: &str,
    version: Option<&str>,
) -> RuntimeCapabilities {
    match (runtime, os, version) {
        (RuntimeKind::Codex, OS_MACOS, Some(CODEX_MACOS_VERSION))
            if cfg!(target_arch = "aarch64") =>
        {
            codex_macos_proven()
        }
        (RuntimeKind::Codex, OS_WINDOWS, Some(CODEX_WIN_VERSION))
        | (RuntimeKind::Codex, OS_LINUX, Some(CODEX_LINUX_VERSION)) => codex_proven(),
        (RuntimeKind::Claude, OS_WINDOWS, Some(CLAUDE_WIN_VERSION))
        | (RuntimeKind::Claude, OS_LINUX, Some(CLAUDE_LINUX_VERSION)) => claude_proven(),
        (RuntimeKind::Opencode, OS_WINDOWS, Some(OPENCODE_WIN_VERSION))
        | (RuntimeKind::Opencode, OS_LINUX, Some(OPENCODE_LINUX_VERSION)) => opencode_proven(),
        _ => unclaimed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_contracts::mission::types::LocalProbeReport;

    #[test]
    fn claude_zai_subscription_launches_from_the_key_store_without_a_connection() {
        let mut binding = crate::agent_runtime::fake::fake_binding();
        binding.runtime = RuntimeKind::Claude;
        binding.provider_id = "zai-coding-plan".into();
        binding.model_id = "glm-5.3".into();
        binding.auth_route = AuthRoute::Subscription;
        binding.credential_ref = None;
        binding.endpoint_ref = None;
        // The key-store route has no compatibility evidence of its own: it
        // stays unclaimed until the user consents for this connection.
        let unconsented = capabilities_for_binding(&binding, OS_MACOS, Some("2.1.276"));
        assert!(!unconsented.structured_result.supported);
        let mut consented = binding.clone();
        consented.experimental_version = Some("2.1.276".into());
        let caps = capabilities_for_binding(&consented, OS_MACOS, Some("2.1.276"));
        assert!(caps.structured_result.supported);
        assert_eq!(
            caps.structured_result.reason_code.as_deref(),
            Some("experimental_opt_in")
        );
    }

    #[test]
    fn macos_evidence_requires_the_proven_model_auth_version_and_architecture() {
        let mut binding = crate::agent_runtime::fake::fake_binding();
        binding.runtime = RuntimeKind::Codex;
        binding.provider_id = "openai".into();
        binding.model_id = CODEX_MACOS_MODEL.into();
        binding.auth_route = AuthRoute::Subscription;
        binding.credential_ref = None;
        binding.endpoint_ref = None;
        let caps = capabilities_for_binding(&binding, OS_MACOS, Some(CODEX_MACOS_VERSION));
        for supported in [
            caps.structured_result.supported,
            caps.events.supported,
            caps.cancel.supported,
            caps.approval_reply.supported,
            caps.read_only.supported,
            caps.scoped_write.supported,
            caps.model_listing.supported,
        ] {
            assert_eq!(supported, cfg!(target_arch = "aarch64"));
        }
        assert!(
            !caps.resume.supported
                && !caps.steer.supported
                && !caps.usage.supported
                && !caps.native_terminal_attach.supported
        );
        for variant in 0..5 {
            let mut other = binding.clone();
            match variant {
                0 => other.model_id = "unproven-model".into(),
                1 => other.auth_route = AuthRoute::ApiKey,
                2 => other.provider_id = "custom".into(),
                3 => other.credential_ref = Some("keyring:test".into()),
                _ => other.endpoint_ref = Some(term_contracts::mission::types::Id::generate()),
            }
            assert_eq!(
                capabilities_for_binding(&other, OS_MACOS, Some(CODEX_MACOS_VERSION)),
                unclaimed()
            );
        }
        for version in [None, Some("0.154.0-beta.1"), Some("0.155.0")] {
            assert_eq!(
                capabilities_for_binding(&binding, OS_MACOS, version),
                unclaimed()
            );
        }
        for os in [OS_LINUX, OS_WINDOWS, "unknown"] {
            assert!(
                !capabilities_for_binding(&binding, os, Some(CODEX_MACOS_VERSION))
                    .scoped_write
                    .supported
            );
        }
    }

    #[test]
    fn macos_live_evidence_reports_are_complete_and_digest_pinned() {
        use sha2::{Digest, Sha256};
        for (bytes, digest, count) in [
            (
                include_bytes!("codex/fixtures/streams/live-session.macos-01540.json").as_slice(),
                CODEX_MACOS_FIXTURE_SHA256,
                3,
            ),
            (
                include_bytes!("codex/fixtures/streams/sandbox.macos-01540.json").as_slice(),
                CODEX_MACOS_SANDBOX_SHA256,
                12,
            ),
        ] {
            assert_eq!(format!("{:x}", Sha256::digest(bytes)), digest);
            let report: serde_json::Value = serde_json::from_slice(bytes).unwrap();
            let cases = report["cases"].as_array().unwrap();
            assert_eq!(cases.len(), count);
            assert!(cases.iter().all(|case| case["passed"] == true));
            if count == 3 {
                assert_eq!(report["version"], CODEX_MACOS_VERSION);
                assert_eq!(report["model"], CODEX_MACOS_MODEL);
                assert_eq!(report["os"], OS_MACOS);
                assert_eq!(report["arch"], "aarch64");
                assert_eq!(report["auth_route"], "subscription");
                assert!(cases
                    .iter()
                    .all(|case| case["cleanup"] == true && case["model_matches"] == true));
            }
        }
    }

    #[test]
    fn detection_hints_are_the_exact_evidence_route_and_model() {
        assert_eq!(evidence_provider_id(RuntimeKind::Fake), None);
        assert_eq!(
            evidence_model_id(RuntimeKind::Codex, OS_MACOS),
            Some(CODEX_MACOS_MODEL)
        );
        for (runtime, os) in [
            (RuntimeKind::Codex, OS_LINUX),
            (RuntimeKind::Codex, OS_WINDOWS),
            (RuntimeKind::Claude, OS_MACOS),
            (RuntimeKind::Opencode, OS_MACOS),
        ] {
            assert_eq!(evidence_model_id(runtime, os), None);
        }
        // A binding built from the hints passes the route gate on the
        // recorded (OS, version) pair; any other provider does not.
        for (runtime, os, version) in [
            (RuntimeKind::Codex, OS_LINUX, CODEX_LINUX_VERSION),
            (RuntimeKind::Claude, OS_LINUX, CLAUDE_LINUX_VERSION),
        ] {
            let mut binding = crate::agent_runtime::fake::fake_binding();
            binding.runtime = runtime;
            binding.provider_id = evidence_provider_id(runtime).unwrap().into();
            binding.auth_route = AuthRoute::Subscription;
            binding.credential_ref = None;
            binding.endpoint_ref = None;
            assert!(
                capabilities_for_binding(&binding, os, Some(version))
                    .events
                    .supported
            );
            binding.provider_id = "custom".into();
            assert_eq!(
                capabilities_for_binding(&binding, os, Some(version)),
                unclaimed()
            );
        }
    }

    #[test]
    fn provider_and_auth_route_cannot_inherit_another_bindings_evidence() {
        let mut binding = crate::agent_runtime::fake::fake_binding();
        binding.runtime = RuntimeKind::Codex;
        binding.provider_id = "openai".into();
        binding.auth_route = AuthRoute::Subscription;
        binding.credential_ref = None;
        binding.endpoint_ref = None;
        assert!(
            capabilities_for_binding(&binding, OS_LINUX, Some(CODEX_LINUX_VERSION))
                .steer
                .supported
        );
        binding.auth_route = AuthRoute::ApiKey;
        assert!(
            !capabilities_for_binding(&binding, OS_LINUX, Some(CODEX_LINUX_VERSION))
                .steer
                .supported
        );
        binding.auth_route = AuthRoute::Subscription;
        binding.provider_id = "custom".into();
        assert!(
            !capabilities_for_binding(&binding, OS_LINUX, Some(CODEX_LINUX_VERSION))
                .steer
                .supported
        );
        binding.provider_id = "openai".into();
        binding.credential_ref = Some("keyring:fixture".into());
        assert!(
            !capabilities_for_binding(&binding, OS_LINUX, Some(CODEX_LINUX_VERSION))
                .steer
                .supported
        );
        binding.credential_ref = None;
        binding.runtime = RuntimeKind::Claude;
        binding.provider_id = "anthropic".into();
        assert!(
            capabilities_for_binding(&binding, OS_WINDOWS, Some(CLAUDE_WIN_VERSION))
                .events
                .supported
        );
        assert!(
            !capabilities_for_binding(&binding, OS_WINDOWS, Some("2.1.263-beta.1"))
                .events
                .supported
        );
        binding.auth_route = AuthRoute::ApiKey;
        assert!(
            !capabilities_for_binding(&binding, OS_WINDOWS, Some(CLAUDE_WIN_VERSION))
                .events
                .supported
        );
    }

    #[test]
    fn evidence_is_os_and_version_pinned() {
        // Windows live versions match.
        assert!(
            capabilities_for(RuntimeKind::Codex, OS_WINDOWS, Some(CODEX_WIN_VERSION))
                .structured_result
                .supported
        );
        // The LINUX version on the WINDOWS key resets — evidence never
        // crosses OS boundaries even for the same CLI.
        assert!(
            !capabilities_for(RuntimeKind::Codex, OS_WINDOWS, Some(CODEX_LINUX_VERSION))
                .structured_result
                .supported
        );
        // Linux live versions match on the linux key.
        let linux = capabilities_for(RuntimeKind::Codex, OS_LINUX, Some(CODEX_LINUX_VERSION));
        assert!(linux.structured_result.supported);
        assert!(linux.cancel.supported);
        assert!(linux.resume.supported);
        // A newer untested version resets to unknown.
        let newer = capabilities_for(RuntimeKind::Codex, OS_LINUX, Some("0.155.0"));
        assert_eq!(
            newer.structured_result.reason_code.as_deref(),
            Some(NO_COMPATIBILITY_EVIDENCE)
        );
        // The Windows version and other macOS runtimes remain unclaimed.
        let mac = capabilities_for(RuntimeKind::Codex, OS_MACOS, Some(CODEX_WIN_VERSION));
        assert!(!mac.structured_result.supported);
        assert!(
            !capabilities_for(RuntimeKind::Claude, OS_MACOS, Some(CLAUDE_WIN_VERSION))
                .resume
                .supported
        );
        assert!(
            !capabilities_for(RuntimeKind::Opencode, OS_MACOS, Some(OPENCODE_WIN_VERSION))
                .model_listing
                .supported
        );
    }

    fn managed(runtime: RuntimeKind) -> Binding {
        let mut binding = crate::agent_runtime::fake::fake_binding();
        binding.runtime = runtime;
        binding.provider_id = evidence_provider_id(runtime).unwrap().into();
        binding.auth_route = AuthRoute::Subscription;
        binding.credential_ref = None;
        binding.endpoint_ref = None;
        binding.experimental_version = None;
        binding
    }

    #[test]
    fn experimental_consent_keeps_evidence_and_grants_only_implemented_capabilities() {
        // Evidence present: proven capabilities stay unmarked, the rest only
        // becomes experimental when the adapter implements it.
        let mut binding = managed(RuntimeKind::Codex);
        binding.experimental_version = Some(CODEX_LINUX_VERSION.into());
        let caps = capabilities_for_binding(&binding, OS_LINUX, Some(CODEX_LINUX_VERSION));
        assert_eq!(caps.events, yes());
        assert_eq!(caps.resume, yes(), "evidence wins over the adapter table");
        assert_eq!(
            caps.scoped_write.reason_code.as_deref(),
            Some(EXPERIMENTAL_OPT_IN)
        );
        assert!(caps.scoped_write.supported);
        assert_eq!(
            caps.native_terminal_attach,
            no_reason(ADAPTER_NOT_IMPLEMENTED)
        );
        // Consent without evidence: implemented → experimental_opt_in.
        let mut claude = managed(RuntimeKind::Claude);
        claude.model_id = "claude-opus-5".into();
        claude.experimental_version = Some("2.1.999".into());
        let caps = capabilities_for_binding(&claude, OS_MACOS, Some("2.1.999"));
        for support in [
            &caps.structured_result,
            &caps.events,
            &caps.cancel,
            &caps.read_only,
            &caps.scoped_write,
            &caps.usage,
        ] {
            assert!(support.supported);
            assert_eq!(support.reason_code.as_deref(), Some(EXPERIMENTAL_OPT_IN));
        }
        for support in [
            &caps.resume,
            &caps.steer,
            &caps.approval_reply,
            &caps.model_listing,
            &caps.native_terminal_attach,
        ] {
            assert_eq!(support, &no_reason(ADAPTER_NOT_IMPLEMENTED));
        }
        for role_kind in [
            term_contracts::mission::types::TaskKind::Plan,
            term_contracts::mission::types::TaskKind::Implement,
            term_contracts::mission::types::TaskKind::Review,
            term_contracts::mission::types::TaskKind::Integrate,
        ] {
            assert_eq!(
                term_core::mission::capability::missing(&caps, role_kind),
                None
            );
        }
    }

    /// Local evidence for this machine's (OS, version, model).
    fn local(
        binding: &Binding,
        os: &str,
        version: &str,
        probe: Option<LocalProbeReport>,
        runs: LocalRunEvidence,
    ) -> LocalEvidence {
        LocalEvidence {
            os: os.into(),
            version: version.into(),
            model_id: binding.model_id.clone(),
            probed_at: Some("2026-09-19T00:00:00Z".into()),
            probe,
            runs,
        }
    }

    fn report(protocol_ok: bool, failures: &[&str]) -> LocalProbeReport {
        LocalProbeReport {
            protocol_ok,
            sandbox_cases_passed: None,
            sandbox_cases_total: None,
            model_listed: None,
            failures: failures.iter().map(|slug| (*slug).to_string()).collect(),
        }
    }

    #[test]
    fn experimental_consent_is_per_connection_and_needs_a_launchable_route() {
        let mut binding = managed(RuntimeKind::Claude);
        binding.experimental_version = Some("2.1.999".into());
        // An updated CLI keeps the consent (11 §3.4): the user re-runs the
        // installation check, never the consent dialog.
        for version in ["2.1.999", "2.1.1000"] {
            let caps = capabilities_for_binding(&binding, OS_MACOS, Some(version));
            assert_eq!(
                caps.structured_result.reason_code.as_deref(),
                Some(EXPERIMENTAL_OPT_IN),
                "{version}"
            );
        }
        // Consent applies even before a version has been observed: it is a
        // statement about this connection, not about one build.
        assert!(
            capabilities_for_binding(&binding, OS_MACOS, None)
                .events
                .supported
        );
        // Empty consent is no consent.
        binding.experimental_version = Some(" ".into());
        assert_eq!(
            capabilities_for_binding(&binding, OS_MACOS, Some("2.1.999")),
            unclaimed()
        );
        // Routes the adapter cannot launch stay unclaimed even with consent.
        binding.experimental_version = Some("2.1.999".into());
        binding.auth_route = AuthRoute::Local;
        assert_eq!(
            capabilities_for_binding(&binding, OS_MACOS, Some("2.1.999")),
            unclaimed()
        );
        let mut opencode = managed(RuntimeKind::Opencode);
        opencode.experimental_version = Some("1.19.0".into());
        assert!(
            !adapter_supports_route(&opencode),
            "OpenCode needs a saved connection"
        );
        opencode.credential_ref = Some("keyring:fixture".into());
        opencode.endpoint_ref = Some(term_contracts::mission::types::Id::generate());
        assert!(adapter_supports_route(&opencode));
        assert!(
            capabilities_for_binding(&opencode, OS_MACOS, Some("1.19.0"))
                .events
                .supported
        );
        let mut fake = crate::agent_runtime::fake::fake_binding();
        fake.experimental_version = fake.runtime_version.clone();
        assert!(!experimental_applies(&fake));
    }

    #[test]
    fn version_lines_follow_major_order_and_known_breaks() {
        // A patch release of the same 0.x line inherits; a new major does not.
        assert!(on_line("0.155.0", "0.154.0", RuntimeKind::Codex));
        assert!(on_line("0.154.0", "0.154.0", RuntimeKind::Codex));
        assert!(on_line("0.154.1", "0.154.0", RuntimeKind::Codex));
        assert!(
            !on_line("0.153.9", "0.154.0", RuntimeKind::Codex),
            "no backfill"
        );
        assert!(!on_line("1.0.0", "0.154.0", RuntimeKind::Codex));
        assert!(!on_line("not-a-version", "0.154.0", RuntimeKind::Codex));
        assert_eq!(parse_semver("2.1"), Some((2, 1, 0)));
        assert_eq!(parse_semver("0.155.0-beta.1+abc"), Some((0, 155, 0)));
        assert_eq!(parse_semver(""), None);
        // A break at `since` ends the line for evidence older than it.
        let breaks = [(RuntimeKind::Codex, "0.155.0")];
        let ev = parse_semver("0.154.0").unwrap();
        assert!(!line_broken(
            RuntimeKind::Codex,
            ev,
            parse_semver("0.154.9").unwrap(),
            &breaks
        ));
        assert!(line_broken(
            RuntimeKind::Codex,
            ev,
            parse_semver("0.155.0").unwrap(),
            &breaks
        ));
        assert!(!line_broken(
            RuntimeKind::Claude,
            ev,
            parse_semver("0.155.0").unwrap(),
            &breaks
        ));
        // Evidence recorded at or after the break is unaffected by it.
        let after = parse_semver("0.155.0").unwrap();
        assert!(!line_broken(
            RuntimeKind::Codex,
            after,
            parse_semver("0.156.0").unwrap(),
            &breaks
        ));
        assert!(KNOWN_BREAKS.is_empty(), "no shipped break today");
        assert_eq!(
            line_evidence_version(RuntimeKind::Codex, OS_LINUX, Some("0.155.0")),
            Some(CODEX_LINUX_VERSION)
        );
        assert_eq!(
            line_evidence_version(RuntimeKind::Codex, OS_MACOS, Some("0.155.0")),
            cfg!(target_arch = "aarch64").then_some(CODEX_MACOS_VERSION)
        );
        assert_eq!(
            line_evidence_version(RuntimeKind::Claude, OS_MACOS, Some("2.1.999")),
            None,
            "no macOS Claude evidence to extend"
        );
    }

    #[test]
    fn the_version_line_applies_only_with_a_passing_protocol_probe() {
        let mut binding = managed(RuntimeKind::Codex);
        binding.model_id = "gpt-anything".into();
        // Newer CLI, no local probe yet: the line is not applied.
        assert_eq!(
            capabilities_for_binding(&binding, OS_LINUX, Some("0.155.0")),
            unclaimed()
        );
        // A failed handshake does not apply it either.
        let failed = local(
            &binding,
            OS_LINUX,
            "0.155.0",
            Some(report(false, &[])),
            LocalRunEvidence::default(),
        );
        binding.local_evidence = Some(failed);
        assert!(
            !capabilities_for_binding(&binding, OS_LINUX, Some("0.155.0"))
                .resume
                .supported
        );
        // Handshake proven: the same-line shipped evidence carries over.
        let proven = local(
            &binding,
            OS_LINUX,
            "0.155.0",
            Some(report(true, &[])),
            LocalRunEvidence::default(),
        );
        binding.local_evidence = Some(proven);
        let caps = capabilities_for_binding(&binding, OS_LINUX, Some("0.155.0"));
        assert_eq!(caps.resume, yes_reason(VERSION_LINE), "shipped-only claim");
        assert_eq!(
            caps.structured_result,
            yes_reason(VERSION_LINE),
            "the line wins over the local probe's own proof"
        );
        // Evidence for a version this local evidence does not describe is
        // ignored: a different observed version resets the machine layer.
        assert_eq!(
            capabilities_for_binding(&binding, OS_LINUX, Some("0.156.0"))
                .resume
                .reason_code
                .as_deref(),
            Some(NO_COMPATIBILITY_EVIDENCE)
        );
        // Another OS never inherits, probe or not.
        assert!(
            !capabilities_for_binding(&binding, "unknown", Some("0.155.0"))
                .resume
                .supported
        );
        // The recorded model pins the exact fixture only: once the protocol
        // check passed, the line applies to any model (11 §8 — model
        // suitability is RESULT_INVALID + plan repair, not a capability).
        if cfg!(target_arch = "aarch64") {
            let mut macos = managed(RuntimeKind::Codex);
            macos.model_id = "gpt-anything".into();
            let probed = local(
                &macos,
                OS_MACOS,
                CODEX_MACOS_VERSION,
                Some(report(true, &[])),
                LocalRunEvidence::default(),
            );
            macos.local_evidence = Some(probed);
            assert_eq!(
                capabilities_for_binding(&macos, OS_MACOS, Some(CODEX_MACOS_VERSION))
                    .approval_reply,
                yes_reason(VERSION_LINE)
            );
            assert_eq!(
                evidence_for_binding(&macos, OS_MACOS, Some(CODEX_MACOS_VERSION)),
                unclaimed(),
                "the shipped fixture itself stays model-pinned"
            );
        }
    }

    #[test]
    fn a_failed_local_check_outranks_consent_and_runs_promote_by_threshold() {
        let mut binding = managed(RuntimeKind::Claude);
        binding.model_id = "claude-opus-5".into();
        binding.experimental_version = Some("2.1.999".into());
        // The help text was read and one flag it needs is absent, so
        // `protocol_ok` is false while the other flags stay proven.
        let missing_flag = local(
            &binding,
            OS_MACOS,
            "2.1.999",
            Some(report(false, &["flag_missing:--json-schema"])),
            LocalRunEvidence::default(),
        );
        binding.local_evidence = Some(missing_flag);
        let caps = capabilities_for_binding(&binding, OS_MACOS, Some("2.1.999"));
        assert_eq!(
            caps.structured_result,
            no_reason(LOCAL_PROBE_FAILED),
            "consent cannot re-open a disproven capability"
        );
        assert_eq!(caps.events, yes_reason(LOCAL_PROBE));
        assert_eq!(caps.read_only, yes_reason(LOCAL_PROBE));
        assert_eq!(caps.steer, no_reason(ADAPTER_NOT_IMPLEMENTED));
        // Run observations promote the capabilities they demonstrated.
        binding.experimental_version = None;
        let runs = |read_only, write, cancelled, invalid| LocalRunEvidence {
            succeeded_read_only: read_only,
            succeeded_write: write,
            cancelled,
            invalid_result: invalid,
            last_at: Some("2026-09-19T00:00:00Z".into()),
        };
        let observed = local(&binding, OS_MACOS, "2.1.999", None, runs(2, 0, 0, 0));
        binding.local_evidence = Some(observed);
        let caps = capabilities_for_binding(&binding, OS_MACOS, Some("2.1.999"));
        assert_eq!(
            caps.structured_result.reason_code.as_deref(),
            Some(NO_COMPATIBILITY_EVIDENCE),
            "two successes are below the threshold"
        );
        let observed = local(&binding, OS_MACOS, "2.1.999", None, runs(2, 1, 1, 0));
        binding.local_evidence = Some(observed);
        let caps = capabilities_for_binding(&binding, OS_MACOS, Some("2.1.999"));
        for support in [
            &caps.structured_result,
            &caps.events,
            &caps.usage,
            &caps.read_only,
            &caps.scoped_write,
            &caps.cancel,
        ] {
            assert_eq!(support, &yes_reason(OBSERVED_RUNS));
        }
        // More invalid results than successes promotes nothing.
        let observed = local(&binding, OS_MACOS, "2.1.999", None, runs(2, 1, 1, 4));
        binding.local_evidence = Some(observed);
        assert_eq!(
            capabilities_for_binding(&binding, OS_MACOS, Some("2.1.999")),
            unclaimed()
        );
        // Counters belong to the model they were collected for.
        let mut other_model = binding.clone();
        other_model.local_evidence = Some(LocalEvidence {
            model_id: "claude-sonnet-5".into(),
            ..local(&binding, OS_MACOS, "2.1.999", None, runs(2, 1, 1, 0))
        });
        assert_eq!(
            capabilities_for_binding(&other_model, OS_MACOS, Some("2.1.999")),
            unclaimed()
        );
    }

    #[test]
    fn compatibility_grades_follow_evidence_probes_and_the_version_line() {
        let grade = |version, shipped_claims, all_roles, protocol_ok| {
            compatibility_grade(
                RuntimeKind::Claude,
                OS_LINUX,
                true,
                version,
                shipped_claims,
                all_roles,
                protocol_ok,
            )
        };
        assert_eq!(
            compatibility_grade(
                RuntimeKind::Claude,
                OS_LINUX,
                false,
                None,
                false,
                false,
                None
            ),
            CompatibilityGrade::NotInstalled
        );
        assert_eq!(
            grade(Some(CLAUDE_LINUX_VERSION), true, false, None),
            CompatibilityGrade::Verified,
            "shipped evidence for this exact version"
        );
        // Same line, no local protocol check yet: one click away from a verdict.
        assert_eq!(
            grade(Some("2.1.999"), false, false, None),
            CompatibilityGrade::SameLineUnverified
        );
        assert_eq!(
            grade(Some("2.2.0"), false, false, Some(false)),
            CompatibilityGrade::SameLineUnverified
        );
        // Line + passing probe + all four roles: the shipped evidence carries it.
        assert_eq!(
            grade(Some("2.1.999"), false, true, Some(true)),
            CompatibilityGrade::Verified
        );
        // No shipped line at all, but the machine proved the four roles.
        assert_eq!(
            compatibility_grade(
                RuntimeKind::Claude,
                OS_MACOS,
                true,
                Some("2.1.270"),
                false,
                true,
                Some(true),
            ),
            CompatibilityGrade::VerifiedLocally,
            "evidence never crosses OS boundaries"
        );
        assert_eq!(
            compatibility_grade(
                RuntimeKind::Claude,
                OS_MACOS,
                true,
                Some("2.1.270"),
                false,
                false,
                Some(true),
            ),
            CompatibilityGrade::Unverified
        );
        assert_eq!(
            grade(Some("3.0.0"), false, false, Some(true)),
            CompatibilityGrade::Unverified,
            "a new major starts a new line"
        );
        assert_eq!(
            compatibility_grade(
                RuntimeKind::Claude,
                OS_MACOS,
                true,
                None,
                false,
                false,
                None
            ),
            CompatibilityGrade::Unverified
        );
    }

    #[test]
    fn per_runtime_claim_sets_are_exact() {
        let claude = capabilities_for(RuntimeKind::Claude, OS_LINUX, Some(CLAUDE_LINUX_VERSION));
        assert!(claude.events.supported);
        assert!(claude.resume.supported);
        assert!(claude.usage.supported);
        assert!(!claude.cancel.supported);
        let oc = capabilities_for(
            RuntimeKind::Opencode,
            OS_LINUX,
            Some(OPENCODE_LINUX_VERSION),
        );
        assert!(oc.model_listing.supported);
        assert!(!oc.events.supported);
    }
}
