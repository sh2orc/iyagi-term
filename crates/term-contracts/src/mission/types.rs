//! O1 mission wire types — Rust mirror of `docs/orchestration/contracts.ts`.
//!
//! Field names, enum spellings, and nullability are the contract; TS bindings
//! are exported via ts-rs and must stay identical to the reference. `null`
//! means unknown/not-applicable; a missing measurement is never zero.
//!
//! Input DTOs (RPC params, provider results) reject unknown fields; entity
//! projections allow forward-compatible extra fields.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::ids::U64String;
use crate::launch::LaunchPolicy;

/// Mission entity identifier: UUID v4. One wire type for every entity kind
/// (reference `contracts.ts` `Id`); semantic mixing is prevented by the
/// domain layer, not by the wire type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub struct Id(String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("Id must be a UUID v4 string, got {0:?}")]
pub struct MissionIdParseError(String);

impl Id {
    pub fn generate() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn parse(s: &str) -> Result<Self, MissionIdParseError> {
        match Uuid::parse_str(s) {
            Ok(u) if u.get_version_num() == 4 => Ok(Self(u.to_string())),
            _ => Err(MissionIdParseError(s.to_string())),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<Id> for String {
    fn from(value: Id) -> Self {
        value.0
    }
}

impl TryFrom<String> for Id {
    type Error = MissionIdParseError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

/// UTC RFC3339 timestamp (wire: string).
pub type Timestamp = String;
/// SHA-256 digest: exactly 64 lowercase hex chars (wire: string).
pub type Sha256 = String;
/// Git object id: 40 (SHA-1) or 64 (SHA-256) hex chars, per repo object format.
pub type GitOid = String;

/// Wire alias — the Rust side reuses the R1 `LaunchPolicy` verbatim
/// (contracts.ts note: "Rust must reuse term_contracts::launch::LaunchPolicy,
/// not define a second one"). Field names already match.
pub type ResourcePolicy = LaunchPolicy;

// ---- enums ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum MissionState {
    Draft,
    Running,
    Pausing,
    Paused,
    Stopping,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Planned,
    Ready,
    Running,
    AwaitingInput,
    AwaitingReview,
    Blocked,
    Succeeded,
    Failed,
    Cancelled,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Prepared,
    Starting,
    Running,
    AwaitingInput,
    Stopping,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Planning,
    Implementing,
    Integrating,
    Validating,
    Reviewing,
    AwaitingAcceptance,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Lead,
    Researcher,
    Architect,
    Builder,
    TestAuthor,
    Reviewer,
    Specialist,
    Diagnostician,
    Integrator,
    Documenter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeKind {
    Codex,
    Claude,
    Opencode,
    Fake,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Plan,
    Research,
    Design,
    Implement,
    TestAuthor,
    Review,
    Consult,
    Diagnose,
    Integrate,
    Document,
    Verify,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AuthRoute {
    Subscription,
    ApiKey,
    Local,
    Custom,
}

// ---- capability & binding ------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Support {
    pub supported: bool,
    pub reason_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RuntimeCapabilities {
    pub structured_result: Support,
    pub events: Support,
    pub cancel: Support,
    pub resume: Support,
    pub steer: Support,
    pub approval_reply: Support,
    pub read_only: Support,
    pub scoped_write: Support,
    pub model_listing: Support,
    pub usage: Support,
    pub native_terminal_attach: Support,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Binding {
    pub id: Id,
    pub revision: U64String,
    pub label: String,
    pub runtime: RuntimeKind,
    /// Absolute executable path, never shell text.
    pub program: String,
    pub runtime_version: Option<String>,
    pub provider_id: String,
    pub model_id: String,
    pub effort: Option<String>,
    pub auth_route: AuthRoute,
    /// Opaque secret-store reference; the secret value never crosses the wire.
    pub credential_ref: Option<String>,
    /// Daemon-owned validated endpoint config id.
    pub endpoint_ref: Option<Id>,
    pub capabilities: RuntimeCapabilities,
    pub checked_at: Option<Timestamp>,
    pub enabled: bool,
    // User-configured per-Run estimate for admission; never a provider hard cap.
    // Older bindings have no estimate. Unknown is not zero.
    #[serde(default)]
    pub estimated_run_cost_usd_micros: Option<U64String>,
    pub resource_policy: ResourcePolicy,
    // Per-connection consent to experimental (implemented but unproven) use.
    // The value is the version observed when the user accepted it, kept for
    // display only — it no longer has to equal the observed version (11 §3.4).
    // Omitted when None so pre-extension binding.save fingerprints still replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental_version: Option<String>,
    // Daemon-owned local evidence (11 §3.3). Omitted when None so older
    // binding.save fingerprints still replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub local_evidence: Option<LocalEvidence>,
}

/// Daemon-measured compatibility evidence for one binding on this machine.
/// Written only by the daemon (`binding.probe`, successful Runs); a value a
/// client sends in `binding.save` is discarded. Applies only while `os` and
/// `version` equal the current installation observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalEvidence {
    pub os: String,
    pub version: String,
    /// Model the run counters were collected for. Probe results are model-independent.
    pub model_id: String,
    #[serde(default)]
    pub probed_at: Option<Timestamp>,
    /// What the no-inference self-check proved; `None` when it has not run for this version.
    #[serde(default)]
    pub probe: Option<LocalProbeReport>,
    #[serde(default)]
    pub runs: LocalRunEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalProbeReport {
    /// The CLI completed the adapter's protocol handshake / advertises every flag the adapter passes.
    pub protocol_ok: bool,
    /// OS-level write/network boundary cases (Codex `command/exec`); `None` when not attempted here.
    #[serde(default)]
    pub sandbox_cases_passed: Option<u32>,
    #[serde(default)]
    pub sandbox_cases_total: Option<u32>,
    /// The binding's model appeared in the runtime's own listing; `None` when the runtime has none.
    #[serde(default)]
    pub model_listed: Option<bool>,
    /// Short fixed slugs of failed checks (never raw CLI output).
    #[serde(default)]
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalRunEvidence {
    #[serde(default)]
    pub succeeded_read_only: u32,
    #[serde(default)]
    pub succeeded_write: u32,
    #[serde(default)]
    pub cancelled: u32,
    #[serde(default)]
    pub invalid_result: u32,
    #[serde(default)]
    pub last_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RoleBinding {
    pub role: Role,
    pub primary_binding_id: Id,
    /// Ordered; user-configured; never inferred.
    pub fallback_binding_ids: Vec<Id>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum UnknownCostPolicy {
    AllowWithNotice,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Policy {
    pub max_parallel_runs: u32,
    pub max_attempts_per_task: u32,
    pub max_repair_cycles: u32,
    pub max_automatic_starts: u32,
    pub active_time_limit_ms: U64String,
    pub run_time_limit_ms: U64String,
    /// `null` = no dollar cap configured (never zero).
    pub max_cost_usd_micros: Option<U64String>,
    pub unknown_cost: UnknownCostPolicy,
    pub allow_network: bool,
    pub allow_automatic_plan_apply: bool,
    pub allow_recovery_of_unsent: bool,
    pub allowed_binding_ids: Vec<Id>,
    pub allowed_roles: Vec<Role>,
    pub allowed_verification_ids: Vec<Id>,
    pub require_independent_review: bool,
    pub require_enforced_verification: bool,
}

// ---- artifacts & requirements ---------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ArtifactRef {
    pub id: Id,
    pub sha256: Sha256,
    pub bytes: U64String,
    pub media_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Requirement {
    pub id: Id,
    pub text: String,
    pub verification_ids: Vec<Id>,
    pub human_check: bool,
}

// ---- projection entities ---------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum ExpectedOutput {
    Report,
    Patch,
    Review,
    Verification,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TaskContract {
    pub objective_ref: ArtifactRef,
    pub requirement_ids: Vec<Id>,
    pub input_artifact_ids: Vec<Id>,
    /// Exact relative path or trailing `/` directory prefix.
    pub allowed_paths: Vec<String>,
    pub expected_outputs: Vec<ExpectedOutput>,
    pub verification_ids: Vec<Id>,
    pub specialty: Option<String>,
}

/// Base recorded from the user's working tree instead of HEAD (04 §1). The
/// mission's `base_oid` is then a daemon-private snapshot commit whose parent
/// is `head_oid`; the user's checkout, index and branches are never written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BaseSnapshot {
    /// Commit the snapshot sits on. Start refuses once HEAD leaves it.
    pub head_oid: GitOid,
    /// Uncommitted entries folded into the base, for display.
    pub entry_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Mission {
    pub id: Id,
    pub revision: U64String,
    // Daemon-owned: the last revision whose commit changed more than
    // housekeeping (time checkpoints, activity timestamps). User mutations
    // accept any expected_revision in [semantic_revision, revision]. Absent in
    // documents written before this field existed; then it equals `revision`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_revision: Option<U64String>,
    pub state: MissionState,
    pub phase: Phase,
    pub title: String,
    pub repository_path: String,
    pub repository_id: Id,
    pub base_oid: GitOid,
    pub goal_ref: ArtifactRef,
    pub requirements: Vec<Requirement>,
    pub policy: Policy,
    pub role_bindings: Vec<RoleBinding>,
    pub plan_revision: u32,
    pub candidate_id: Option<Id>,
    pub open_decision_count: u32,
    pub active_time_ms: U64String,
    pub automatic_start_count: u32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub archived_at: Option<Timestamp>,
    pub accepted_at: Option<Timestamp>,
    pub failure_code: Option<super::error::MissionErrorCode>,
    // Accepted mission whose candidate commit is this mission's base.
    #[serde(default)]
    pub follow_up_of: Option<Id>,
    // Present when `base_oid` is a working-tree snapshot rather than HEAD.
    // Absent in documents written before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_snapshot: Option<BaseSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IntegrationTask {
    pub plan_ref: ArtifactRef,
    pub step: IntegrationStep,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationStep {
    Automatic,
    Resolving {
        conflict_run_id: Id,
    },
    Continuing {
        conflict_run_id: Id,
        resolution_run_id: Id,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Task {
    pub id: Id,
    pub mission_id: Id,
    pub title: String,
    pub kind: TaskKind,
    pub role: Option<Role>,
    pub state: TaskState,
    pub required: bool,
    pub parent_task_id: Option<Id>,
    pub depends_on: Vec<Id>,
    pub contract: TaskContract,
    pub binding_id: Option<Id>,
    pub active_run_id: Option<Id>,
    pub ordinal: u32,
    pub attempt_count: u32,
    pub repair_cycle: u32,
    // Daemon-owned link from a Lead repair task to exact failed executions.
    #[serde(default)]
    pub failure_repair_run_ids: Vec<Id>,
    // Daemon-owned integration execution mode; never supplied by a planner.
    #[serde(default)]
    pub integration: Option<IntegrationTask>,
    pub replacement_of: Option<Id>,
    pub blocked_code: Option<String>,
    // Known provider reset or automatic retry deadline; null if timing is unknown.
    #[serde(default)]
    pub dispatch_after_unix_ms: Option<U64String>,
    pub workspace_id: Option<Id>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Task {
    /// Includes legacy automatic tasks written before integration modes existed.
    pub fn is_internal_integration(&self) -> bool {
        self.kind == TaskKind::Integrate
            && (self.integration.is_some() || (self.role.is_none() && self.binding_id.is_none()))
    }

    pub fn is_resolving_integration(&self) -> bool {
        self.is_internal_integration()
            && self
                .integration
                .as_ref()
                .is_some_and(|i| matches!(i.step, IntegrationStep::Resolving { .. }))
    }

    pub fn is_deterministic_integration(&self) -> bool {
        self.is_internal_integration() && !self.is_resolving_integration()
    }

    pub fn execution_binding_id(&self) -> Option<&Id> {
        if self.is_deterministic_integration() {
            None
        } else {
            self.binding_id.as_ref()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum UsageCostSource {
    Provider,
    Estimate,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Usage {
    pub input_tokens: Option<U64String>,
    pub output_tokens: Option<U64String>,
    pub cost_usd_micros: Option<U64String>,
    pub cost_source: UsageCostSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum RunDispatchState {
    Unsent,
    MayHaveSent,
    Acknowledged,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Run {
    pub id: Id,
    pub mission_id: Id,
    pub task_id: Id,
    pub attempt: u32,
    pub state: RunState,
    /// Null for deterministic verification/integration runs.
    pub binding_snapshot: Option<Binding>,
    pub requested_model: Option<String>,
    pub observed_model: Option<String>,
    pub provider_session_id: Option<String>,
    pub provider_turn_id: Option<String>,
    pub exec_id: Option<Id>,
    pub pty_session_id: Option<String>,
    pub workspace_id: Option<Id>,
    pub fencing_token: U64String,
    pub dispatch_state: RunDispatchState,
    pub context_ref: ArtifactRef,
    pub result_ref: Option<ArtifactRef>,
    pub usage: Usage,
    pub last_activity_at: Option<Timestamp>,
    pub active_time_ms: U64String,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub failure_code: Option<super::error::MissionErrorCode>,
    // Daemon-authored proof of local execution termination; not provider success.
    pub reconciliation_ref: Option<ArtifactRef>,
    #[serde(default)]
    pub rate_limit: Option<RateLimitObservation>,
    // Adapter/validator evidence, distinct from an error's transport retryability.
    #[serde(default)]
    pub retry_evidence: Option<RetryEvidence>,
    // What `reconciliation_ref` proves: an observed durable exit or only the
    // user's attestation. Absent on legacy proofs (observed exit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation_kind: Option<ReconciliationKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationKind {
    ExecExited,
    UserAttested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "basis", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetryEvidence {
    RequestNotSubmitted {
        observed_at_unix_ms: U64String,
        retry_after_unix_ms: Option<U64String>,
    },
    PlanFormatRejected {
        plan_revision: u32,
        rejected_result_ref: Option<ArtifactRef>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RateLimitObservation {
    pub observed_at_unix_ms: U64String,
    pub resets_at_unix_ms: U64String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Agent,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum MessageDelivery {
    Queued,
    Delivered,
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Message {
    pub id: Id,
    pub mission_id: Id,
    /// Null target = the Lead.
    pub target_task_id: Option<Id>,
    pub role: MessageRole,
    pub run_id: Option<Id>,
    pub body_ref: ArtifactRef,
    pub delivery: MessageDelivery,
    pub supersedes_message_id: Option<Id>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum ExecState {
    Prepared,
    Spawned,
    Stopping,
    Exited,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ExecGroupKind {
    Cgroup,
    Job,
    ObservedTree,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExecRecord {
    pub id: Id,
    pub mission_id: Id,
    pub run_id: Id,
    pub state: ExecState,
    pub identity: Option<crate::ids::ProcessIdentity>,
    pub group_kind: Option<ExecGroupKind>,
    pub group_reference: Option<String>,
    #[serde(default)]
    pub group_identity: Option<crate::workload::GroupRecoveryIdentity>,
    pub resource_policy: ResourcePolicy,
    // Sanitized program/argv snapshot; no secret env values.
    pub launch_manifest_ref: ArtifactRef,
    pub owner_daemon_id: Id,
    pub started_at: Option<Timestamp>,
    pub ended_at: Option<Timestamp>,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum DecisionKind {
    Product,
    Approval,
    Budget,
    Recovery,
    Plan,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum DecisionState {
    Open,
    Answered,
    Obsolete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DecisionOption {
    /// Stable slug, not a UUID.
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Decision {
    pub id: Id,
    pub mission_id: Id,
    pub requesting_run_id: Option<Id>,
    pub kind: DecisionKind,
    pub state: DecisionState,
    pub question_ref: ArtifactRef,
    pub options: Vec<DecisionOption>,
    pub affected_task_ids: Vec<Id>,
    pub blocking: bool,
    pub plan_revision: u32,
    pub candidate_id: Option<Id>,
    pub answer_ref: Option<ArtifactRef>,
    pub selected_option_id: Option<String>,
    pub answer_message_id: Option<Id>,
    pub created_at: Timestamp,
    pub answered_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceKind {
    Worker,
    Integration,
    Verification,
    Experiment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceState {
    Preparing,
    Ready,
    Busy,
    Quarantined,
    Retained,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Workspace {
    pub id: Id,
    pub mission_id: Id,
    pub path: String,
    pub kind: WorkspaceKind,
    pub base_oid: GitOid,
    pub head_oid: GitOid,
    pub writer_run_id: Option<Id>,
    pub lease_token: U64String,
    pub state: WorkspaceState,
    pub owned_by_daemon: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Candidate {
    pub id: Id,
    pub mission_id: Id,
    pub revision: u32,
    pub base_oid: GitOid,
    pub tree_oid: GitOid,
    pub commit_oid: GitOid,
    pub source_run_ids: Vec<Id>,
    pub manifest_ref: ArtifactRef,
    pub created_at: Timestamp,
    pub supersedes_id: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VerificationCommand {
    pub id: Id,
    pub title: String,
    pub program: String,
    pub argv: Vec<String>,
    pub revision: U64String,
    pub repository_id: Id,
    pub cwd_relative: String,
    pub timeout_ms: u64,
    pub env_profile_ref: Option<Id>,
    pub allowed_network: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum VerificationStatus {
    Passed,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum InputIntegrity {
    Enforced,
    Observed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Verification {
    pub id: Id,
    pub mission_id: Id,
    pub candidate_id: Id,
    pub task_id: Id,
    pub run_id: Id,
    pub command_snapshot_ref: ArtifactRef,
    pub environment_ref: ArtifactRef,
    pub requirement_ids: Vec<Id>,
    pub status: VerificationStatus,
    pub input_integrity: InputIntegrity,
    pub exit_code: Option<i32>,
    pub log_ref: ArtifactRef,
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    Blocking,
    Major,
    Minor,
    Note,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum FindingResolution {
    Open,
    Fixed,
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Finding {
    pub id: Id,
    pub mission_id: Id,
    pub candidate_id: Id,
    pub reviewer_run_id: Id,
    pub severity: FindingSeverity,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub evidence_ref: ArtifactRef,
    pub requirement_id: Option<Id>,
    pub resolution: FindingResolution,
    pub resolution_ref: Option<ArtifactRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum KnowledgeKind {
    Fact,
    Hypothesis,
    Decision,
    Question,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum KnowledgeStatus {
    Proposed,
    Accepted,
    Stale,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Knowledge {
    pub id: Id,
    pub mission_id: Id,
    pub kind: KnowledgeKind,
    pub body_ref: ArtifactRef,
    pub source_artifact_ids: Vec<Id>,
    pub source_run_id: Option<Id>,
    pub base_oid: GitOid,
    pub related_paths: Vec<String>,
    pub status: KnowledgeStatus,
    pub supersedes_id: Option<Id>,
}

// ---- context, plans, teams -------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContextOmission {
    pub artifact_id: Id,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ContextBundle {
    pub version: u32,
    pub mission_id: Id,
    pub task_id: Id,
    pub base_oid: GitOid,
    pub goal_ref: ArtifactRef,
    pub contract: TaskContract,
    pub artifacts: Vec<ArtifactRef>,
    pub knowledge_ids: Vec<Id>,
    pub prior_run_ids: Vec<Id>,
    pub omitted: Vec<ContextOmission>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TaskSpec {
    pub id: Id,
    pub title: String,
    pub kind: TaskKind,
    pub role: Option<Role>,
    pub required: bool,
    pub parent_task_id: Option<Id>,
    pub depends_on: Vec<Id>,
    pub contract: TaskContract,
    pub binding_id: Option<Id>,
    pub replacement_of: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PlanProposal {
    pub id: Id,
    pub mission_id: Id,
    pub based_on_plan_revision: u32,
    pub tasks: Vec<TaskSpec>,
    pub retire_task_ids: Vec<Id>,
    pub rationale_ref: ArtifactRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TeamTemplate {
    pub id: Id,
    pub revision: U64String,
    pub label: String,
    pub repository_id: Option<Id>,
    pub role_bindings: Vec<RoleBinding>,
    pub policy: Policy,
}

/// Task spec as the model proposes it. `local_key` is the provider-local
/// name; the daemon allocates the UUID exactly once (02 §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ProviderTaskSpec {
    /// ASCII `[a-z][a-z0-9_-]{0,63}`.
    pub local_key: String,
    pub title: String,
    pub kind: TaskKind,
    pub role: Option<Role>,
    pub required: bool,
    pub parent_key: Option<String>,
    pub depends_on_keys: Vec<String>,
    pub objective_text: String,
    pub requirement_ids: Vec<Id>,
    pub input_artifact_ids: Vec<Id>,
    pub allowed_paths: Vec<String>,
    pub expected_outputs: Vec<ExpectedOutput>,
    pub verification_ids: Vec<Id>,
    pub specialty: Option<String>,
    pub binding_id: Option<Id>,
    pub replacement_of: Option<Id>,
}

/// Provider-side knowledge draft (no fabricated ids, plain text bodies).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ProviderKnowledgeDraft {
    pub kind: KnowledgeKind,
    pub text: String,
    pub source_artifact_ids: Vec<Id>,
    pub related_paths: Vec<String>,
}

/// Provider-side finding draft (evidence as text; daemon mints the Finding).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ProviderFindingDraft {
    pub severity: FindingSeverity,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub evidence_text: String,
    pub requirement_id: Option<Id>,
}

/// Model output DTO. Bodies are plain text; the adapter registers artifacts
/// and the daemon converts to the wire [`AgentResult`] after resolving every
/// referenced id (03 §2). Fabricated artifact ids are a RESULT_INVALID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ProviderResult {
    Plan {
        based_on_plan_revision: u32,
        tasks: Vec<ProviderTaskSpec>,
        retire_task_ids: Vec<Id>,
        rationale_text: String,
    },
    Report {
        report_text: String,
        knowledge: Vec<ProviderKnowledgeDraft>,
    },
    Patch {
        report_text: String,
        verification_claims: Vec<String>,
    },
    Review {
        candidate_id: Id,
        report_text: String,
        findings: Vec<ProviderFindingDraft>,
    },
    Question {
        question_text: String,
        options: Vec<DecisionOption>,
    },
    Blocked {
        code: String,
        report_text: String,
    },
}

/// Daemon-validated agent result: every reference resolved to a real
/// daemon-issued artifact/entity id. Distinct from [`ProviderResult`] by
/// contract (02: "별개 enum").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AgentResult {
    Plan {
        proposal: PlanProposal,
    },
    Report {
        report_ref: ArtifactRef,
        knowledge: Vec<Knowledge>,
    },
    Patch {
        report_ref: ArtifactRef,
        verification_claims: Vec<String>,
    },
    Review {
        candidate_id: Id,
        findings: Vec<Finding>,
        report_ref: ArtifactRef,
    },
    Question {
        question_ref: ArtifactRef,
        options: Vec<DecisionOption>,
    },
    Blocked {
        code: String,
        report_ref: ArtifactRef,
    },
}

// ---- events & snapshot -----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum EntityKind {
    Mission,
    Task,
    Run,
    Message,
    Decision,
    Workspace,
    Candidate,
    Verification,
    Finding,
    Knowledge,
    Exec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum ChangeOperation {
    Upsert,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Change {
    pub entity_kind: EntityKind,
    pub entity_id: Id,
    /// Delete only expires content projections, never history.
    pub operation: ChangeOperation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum MissionEventType {
    Created,
    Changed,
    PlanApplied,
    DecisionAnswered,
    RunDispatched,
    Reconciled,
    Accepted,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MissionEvent {
    pub mission_id: Id,
    pub seq: U64String,
    pub revision: U64String,
    pub transaction_id: Id,
    #[serde(rename = "type")]
    pub event_type: MissionEventType,
    /// Exactly one of `changes` / `changes_ref` is non-null.
    pub changes: Option<Vec<Change>>,
    pub changes_ref: Option<ArtifactRef>,
    pub created_at: Timestamp,
}

/// Snapshot entity union: `{kind, value}` on the wire. serde's adjacently
/// tagged representation rejects tuple variants, so the conversions are
/// written by hand to keep the ergonomic `Entity::Task(task)` shape.
///
/// The `ts` attributes exist because those hand-written conversions are
/// invisible to ts-rs: without them the export falls back to serde's
/// *external* tagging (`{"Task": …}`), which no peer has ever sent or
/// accepted. A client written against that shape drops every entity of every
/// snapshot silently — the whole mission view reads as empty while the daemon
/// is working normally — so the tags below must keep matching the `Serialize`
/// impl and `EntityKind` exactly.
#[derive(Debug, Clone, PartialEq, TS)]
#[ts(export)]
#[ts(tag = "kind", content = "value")]
#[ts(rename_all = "lowercase")]
pub enum Entity {
    Mission(Box<Mission>),
    Task(Box<Task>),
    Run(Box<Run>),
    Message(Box<Message>),
    Decision(Box<Decision>),
    Workspace(Box<Workspace>),
    Candidate(Box<Candidate>),
    Verification(Box<Verification>),
    Finding(Box<Finding>),
    Knowledge(Box<Knowledge>),
    Exec(Box<ExecRecord>),
}

impl Serialize for Entity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let kind = match self {
            Entity::Mission(_) => "mission",
            Entity::Task(_) => "task",
            Entity::Run(_) => "run",
            Entity::Message(_) => "message",
            Entity::Decision(_) => "decision",
            Entity::Workspace(_) => "workspace",
            Entity::Candidate(_) => "candidate",
            Entity::Verification(_) => "verification",
            Entity::Finding(_) => "finding",
            Entity::Knowledge(_) => "knowledge",
            Entity::Exec(_) => "exec",
        };
        let value = match self {
            Entity::Mission(v) => serde_json::to_value(v),
            Entity::Task(v) => serde_json::to_value(v),
            Entity::Run(v) => serde_json::to_value(v),
            Entity::Message(v) => serde_json::to_value(v),
            Entity::Decision(v) => serde_json::to_value(v),
            Entity::Workspace(v) => serde_json::to_value(v),
            Entity::Candidate(v) => serde_json::to_value(v),
            Entity::Verification(v) => serde_json::to_value(v),
            Entity::Finding(v) => serde_json::to_value(v),
            Entity::Knowledge(v) => serde_json::to_value(v),
            Entity::Exec(v) => serde_json::to_value(v),
        }
        .map_err(serde::ser::Error::custom)?;
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("kind", kind)?;
        map.serialize_entry("value", &value)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for Entity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct RawEntity {
            kind: String,
            value: serde_json::Value,
        }
        let raw = RawEntity::deserialize(deserializer)?;
        let error = |message: &str| serde::de::Error::custom(message.to_string());
        match raw.kind.as_str() {
            "mission" => Ok(Entity::Mission(
                serde_json::from_value(raw.value).map_err(|e| error(&e.to_string()))?,
            )),
            "task" => Ok(Entity::Task(
                serde_json::from_value(raw.value).map_err(|e| error(&e.to_string()))?,
            )),
            "run" => Ok(Entity::Run(
                serde_json::from_value(raw.value).map_err(|e| error(&e.to_string()))?,
            )),
            "message" => Ok(Entity::Message(
                serde_json::from_value(raw.value).map_err(|e| error(&e.to_string()))?,
            )),
            "decision" => Ok(Entity::Decision(
                serde_json::from_value(raw.value).map_err(|e| error(&e.to_string()))?,
            )),
            "workspace" => Ok(Entity::Workspace(
                serde_json::from_value(raw.value).map_err(|e| error(&e.to_string()))?,
            )),
            "candidate" => Ok(Entity::Candidate(
                serde_json::from_value(raw.value).map_err(|e| error(&e.to_string()))?,
            )),
            "verification" => Ok(Entity::Verification(
                serde_json::from_value(raw.value).map_err(|e| error(&e.to_string()))?,
            )),
            "finding" => Ok(Entity::Finding(
                serde_json::from_value(raw.value).map_err(|e| error(&e.to_string()))?,
            )),
            "knowledge" => Ok(Entity::Knowledge(
                serde_json::from_value(raw.value).map_err(|e| error(&e.to_string()))?,
            )),
            "exec" => Ok(Entity::Exec(
                serde_json::from_value(raw.value).map_err(|e| error(&e.to_string()))?,
            )),
            other => Err(error(&format!("unknown entity kind {other:?}"))),
        }
    }
}

impl Entity {
    pub fn kind(&self) -> EntityKind {
        match self {
            Entity::Mission(_) => EntityKind::Mission,
            Entity::Task(_) => EntityKind::Task,
            Entity::Run(_) => EntityKind::Run,
            Entity::Message(_) => EntityKind::Message,
            Entity::Decision(_) => EntityKind::Decision,
            Entity::Workspace(_) => EntityKind::Workspace,
            Entity::Candidate(_) => EntityKind::Candidate,
            Entity::Verification(_) => EntityKind::Verification,
            Entity::Finding(_) => EntityKind::Finding,
            Entity::Knowledge(_) => EntityKind::Knowledge,
            Entity::Exec(_) => EntityKind::Exec,
        }
    }

    pub fn entity_id(&self) -> Id {
        match self {
            Entity::Mission(v) => v.id.clone(),
            Entity::Task(v) => v.id.clone(),
            Entity::Run(v) => v.id.clone(),
            Entity::Message(v) => v.id.clone(),
            Entity::Decision(v) => v.id.clone(),
            Entity::Workspace(v) => v.id.clone(),
            Entity::Candidate(v) => v.id.clone(),
            Entity::Verification(v) => v.id.clone(),
            Entity::Finding(v) => v.id.clone(),
            Entity::Knowledge(v) => v.id.clone(),
            Entity::Exec(v) => v.id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SnapshotPage {
    pub snapshot_id: Id,
    pub mission_id: Id,
    pub at_seq: U64String,
    pub revision: U64String,
    pub entities: Vec<Entity>,
    /// Opaque cursor bound to this snapshot; null on the last page.
    pub next_cursor: Option<String>,
    pub expires_at: Timestamp,
}

// ---- mutation envelope -----------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Mutation {
    pub request_id: Id,
    pub mission_id: Id,
    pub expected_revision: U64String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct MutationResult {
    pub mission_id: Id,
    pub revision: U64String,
    pub event_seq: U64String,
    pub entity_ids: Vec<Id>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_requires_uuid_v4() {
        let id = Id::generate();
        assert!(Id::parse(id.as_str()).is_ok());
        assert!(Id::parse("not-a-uuid").is_err());
        assert!(Id::parse("e2f5c8e0-6b1a-11d0-a08c-0020af31e880").is_err()); // v1
    }

    #[test]
    fn state_enum_wire_names() {
        assert_eq!(
            serde_json::to_string(&MissionState::Draft).unwrap(),
            "\"draft\""
        );
        assert_eq!(
            serde_json::to_string(&TaskState::AwaitingInput).unwrap(),
            "\"awaiting_input\""
        );
        assert_eq!(
            serde_json::to_string(&RunState::AwaitingInput).unwrap(),
            "\"awaiting_input\""
        );
        assert_eq!(
            serde_json::to_string(&Phase::AwaitingAcceptance).unwrap(),
            "\"awaiting_acceptance\""
        );
        assert_eq!(
            serde_json::to_string(&Role::TestAuthor).unwrap(),
            "\"test_author\""
        );
        assert_eq!(
            serde_json::to_string(&TaskKind::TestAuthor).unwrap(),
            "\"test_author\""
        );
        assert_eq!(
            serde_json::to_string(&AuthRoute::ApiKey).unwrap(),
            "\"api_key\""
        );
        assert_eq!(
            serde_json::to_string(&RuntimeKind::Opencode).unwrap(),
            "\"opencode\""
        );
        assert_eq!(
            serde_json::to_string(&RunDispatchState::MayHaveSent).unwrap(),
            "\"may_have_sent\""
        );
        assert_eq!(
            serde_json::to_string(&ExecGroupKind::ObservedTree).unwrap(),
            "\"observed_tree\""
        );
        assert_eq!(
            serde_json::to_string(&UnknownCostPolicy::AllowWithNotice).unwrap(),
            "\"allow_with_notice\""
        );
        assert_eq!(
            serde_json::to_string(&InputIntegrity::Enforced).unwrap(),
            "\"enforced\""
        );
        assert_eq!(
            serde_json::to_string(&FindingSeverity::Blocking).unwrap(),
            "\"blocking\""
        );
        assert_eq!(
            serde_json::to_string(&VerificationStatus::Passed).unwrap(),
            "\"passed\""
        );
    }

    #[test]
    fn entity_serializes_as_kind_value_pair() {
        let mission = Mission {
            id: Id::generate(),
            revision: U64String::new(1).unwrap(),
            semantic_revision: None,
            state: MissionState::Draft,
            phase: Phase::Planning,
            title: "t".into(),
            repository_path: "/repo".into(),
            repository_id: Id::generate(),
            base_oid: "a".repeat(40),
            goal_ref: ArtifactRef {
                id: Id::generate(),
                sha256: "b".repeat(64),
                bytes: U64String::new(1).unwrap(),
                media_type: "text/plain".into(),
            },
            requirements: Vec::new(),
            policy: test_policy(),
            role_bindings: Vec::new(),
            plan_revision: 0,
            candidate_id: None,
            open_decision_count: 0,
            active_time_ms: U64String::new(0).unwrap(),
            automatic_start_count: 0,
            created_at: "2026-09-13T00:00:00Z".into(),
            updated_at: "2026-09-13T00:00:00Z".into(),
            archived_at: None,
            accepted_at: None,
            failure_code: None,
            follow_up_of: None,
            base_snapshot: None,
        };
        let entity = Entity::Mission(Box::new(mission));
        let json = serde_json::to_value(&entity).unwrap();
        assert_eq!(json["kind"], "mission");
        assert!(json["value"]["id"].is_string());
        let back: Entity = serde_json::from_value(json).unwrap();
        assert_eq!(back.entity_id(), entity.entity_id());
        assert_eq!(back.kind(), EntityKind::Mission);
    }

    #[test]
    fn provider_result_is_strictly_tagged() {
        let json = serde_json::json!({
            "kind": "question",
            "question_text": "어느 범위를 지원할까요?",
            "options": [{"id": "v1", "label": "v1만"}, {"id": "both", "label": "둘 다"}]
        });
        let result: ProviderResult = serde_json::from_value(json).unwrap();
        match &result {
            ProviderResult::Question { options, .. } => assert_eq!(options.len(), 2),
            other => panic!("unexpected variant {other:?}"),
        }
        // Unknown fields are rejected on model output.
        let junk = serde_json::json!({
            "kind": "report", "report_text": "x", "knowledge": [], "surprise": 1
        });
        assert!(serde_json::from_value::<ProviderResult>(junk).is_err());
        // Missing kind tag is rejected.
        assert!(serde_json::from_value::<ProviderResult>(serde_json::json!({
            "report_text": "x"
        }))
        .is_err());
    }

    #[test]
    fn agent_result_variants_match_contract() {
        let json = serde_json::json!({
            "kind": "patch",
            "report_ref": {"id": Id::generate().to_string(), "sha256": "c".repeat(64),
                           "bytes": "3", "media_type": "text/plain"},
            "verification_claims": ["npm test"]
        });
        let result: AgentResult = serde_json::from_value(json).unwrap();
        assert!(matches!(result, AgentResult::Patch { .. }));
    }

    #[test]
    fn mission_event_field_is_named_type_on_wire() {
        let event = MissionEvent {
            mission_id: Id::generate(),
            seq: U64String::new(1).unwrap(),
            revision: U64String::new(1).unwrap(),
            transaction_id: Id::generate(),
            event_type: MissionEventType::PlanApplied,
            changes: Some(Vec::new()),
            changes_ref: None,
            created_at: "2026-09-13T00:00:00Z".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "plan_applied");
    }

    fn test_policy() -> Policy {
        Policy {
            max_parallel_runs: 4,
            max_attempts_per_task: 3,
            max_repair_cycles: 3,
            max_automatic_starts: 64,
            active_time_limit_ms: U64String::new(14_400_000).unwrap(),
            run_time_limit_ms: U64String::new(2_700_000).unwrap(),
            max_cost_usd_micros: None,
            unknown_cost: UnknownCostPolicy::AllowWithNotice,
            allow_network: false,
            allow_automatic_plan_apply: true,
            allow_recovery_of_unsent: true,
            allowed_binding_ids: Vec::new(),
            allowed_roles: Vec::new(),
            allowed_verification_ids: Vec::new(),
            require_independent_review: true,
            require_enforced_verification: false,
        }
    }
}
