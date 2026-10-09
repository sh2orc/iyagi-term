//! O1 mission RPC method names, params, and results (reference
//! `contracts.ts` `Rpc`). Params are strict (`deny_unknown_fields`); result
//! payloads follow the entity projection forward-compatibility rule.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::types::{
    ArtifactRef, Binding, Id, Mission, MissionEvent, Policy, Requirement, Role, RoleBinding,
    RuntimeKind, TeamTemplate, VerificationCommand,
};
use crate::ids::U64String;

/// Well-known O1 method names (dispatcher keys).
pub mod methods {
    pub const REPOSITORY_INSPECT: &str = "repository.inspect";
    pub const MISSION_CREATE: &str = "mission.create";
    pub const MISSION_LIST: &str = "mission.list";
    pub const MISSION_SNAPSHOT: &str = "mission.snapshot";
    pub const MISSION_EVENTS: &str = "mission.events";
    pub const MISSION_CONTROL: &str = "mission.control";
    pub const MISSION_ACCEPT: &str = "mission.accept";
    pub const MISSION_MESSAGE: &str = "mission.message";
    pub const MISSION_PLAN_APPLY: &str = "mission.plan.apply";
    pub const MISSION_TASK_CONTROL: &str = "mission.task.control";
    pub const MISSION_POLICY_UPDATE: &str = "mission.policy.update";
    pub const MISSION_FINDING_RESOLVE: &str = "mission.finding.resolve";
    pub const MISSION_DECISION_ANSWER: &str = "mission.decision.answer";
    pub const MISSION_REQUEST_GET: &str = "mission.request.get";
    pub const MISSION_ACTIVITY: &str = "mission.activity";
    pub const MISSION_RUN_ATTEST_EXITED: &str = "mission.run.attest_exited";
    pub const BINDING_LIST: &str = "binding.list";
    pub const BINDING_SAVE: &str = "binding.save";
    pub const BINDING_PROBE: &str = "binding.probe";
    // Not an RPC: the save-source tag a Run observation uses when it updates
    // a binding document's local evidence (11 §3.5).
    pub const BINDING_RUN_EVIDENCE: &str = "binding.run_evidence";
    pub const RUNTIME_DETECT: &str = "runtime.detect";
    pub const TEMPLATE_LIST: &str = "template.list";
    pub const TEMPLATE_SAVE: &str = "template.save";
    pub const VERIFICATION_LIST: &str = "verification.list";
    pub const VERIFICATION_SAVE: &str = "verification.save";
    pub const ARTIFACT_BEGIN: &str = "artifact.begin";
    pub const ARTIFACT_WRITE: &str = "artifact.write";
    pub const ARTIFACT_COMMIT: &str = "artifact.commit";
    pub const ARTIFACT_READ: &str = "artifact.read";
    pub const WORKSPACE_USAGE: &str = "workspace.usage";
    pub const WORKSPACE_CLEANUP: &str = "workspace.cleanup";
}

/// `mission.changed` notification payload — a hint, never proof (01 §4).
pub const EVENT_MISSION_CHANGED: &str = "mission.changed";

// ---- mission methods -------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct RepositoryInspectParams {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RepositoryInspectResult {
    pub repository_id: Id,
    pub canonical_path: String,
    pub head_oid: String,
    pub clean: bool,
    pub dirty_paths: Vec<String>,
    // True only when this daemon's OS has the isolated verification executor;
    // selecting verification commands on other platforms cannot be accepted.
    #[serde(default)]
    pub verification_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionCreateParams {
    pub request_id: Id,
    pub title: String,
    pub repository_path: String,
    pub expected_base_oid: String,
    pub goal_ref: ArtifactRef,
    pub requirements: Vec<Requirement>,
    pub policy: Policy,
    pub role_bindings: Vec<RoleBinding>,
    // Completed mission whose accepted candidate commit is the new base.
    // Omitted when None so pre-extension request fingerprints still replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub follow_up_of: Option<Id>,
    // Record the working tree (including untracked, never ignored, files) as
    // the base instead of refusing a dirty repository. Omitted when None so
    // pre-extension request fingerprints still replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub include_uncommitted: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionListParams {
    pub cursor: Option<String>,
    pub limit: u32,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionListResult {
    pub items: Vec<Mission>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionSnapshotParams {
    pub mission_id: Id,
    pub snapshot_id: Option<Id>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionEventsParams {
    pub mission_id: Id,
    pub after_seq: U64String,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionEventsResult {
    pub events: Vec<MissionEvent>,
    pub high_watermark: U64String,
    pub next_after_seq: U64String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum MissionControlAction {
    Start,
    Pause,
    Resume,
    Cancel,
    Archive,
    Unarchive,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionControlParams {
    pub request_id: Id,
    pub mission_id: Id,
    pub expected_revision: U64String,
    pub action: MissionControlAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionAcceptParams {
    pub request_id: Id,
    pub mission_id: Id,
    pub expected_revision: U64String,
    pub candidate_id: Id,
    pub acknowledged_verification_ids: Vec<Id>,
    pub human_requirement_ids: Vec<Id>,
    // Explicit review of past uncertain effects; absence preserves the old gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub acknowledged_reconciled_run_ids: Option<Vec<Id>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionMessageParams {
    pub request_id: Id,
    pub mission_id: Id,
    pub expected_revision: U64String,
    /// Null target = the Lead conversation.
    pub target_task_id: Option<Id>,
    pub body_ref: ArtifactRef,
    // Explicit replacement of a user's unknown/rejected instruction. Omit
    // None during serialization to preserve pre-extension request fingerprints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub supersedes_message_id: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionPlanApplyParams {
    pub request_id: Id,
    pub mission_id: Id,
    pub expected_revision: U64String,
    pub proposal_ref: ArtifactRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum TaskControlAction {
    Cancel,
    Retry,
    Reassign,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionTaskControlParams {
    pub request_id: Id,
    pub mission_id: Id,
    pub expected_revision: U64String,
    pub task_id: Id,
    pub action: TaskControlAction,
    // Reassign selects a binding; Retry may atomically select it for the new attempt.
    pub binding_id: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionPolicyUpdateParams {
    pub request_id: Id,
    pub mission_id: Id,
    pub expected_revision: U64String,
    pub policy: Policy,
    pub role_bindings: Vec<RoleBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionFindingResolveParams {
    pub request_id: Id,
    pub mission_id: Id,
    pub expected_revision: U64String,
    pub finding_id: Id,
    pub resolution: FindingResolutionAction,
    pub reason_ref: ArtifactRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum FindingResolutionAction {
    /// Users may only dismiss; `fixed` is minted by the engine (01 §3).
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionDecisionAnswerParams {
    pub request_id: Id,
    pub mission_id: Id,
    pub expected_revision: U64String,
    pub decision_id: Id,
    pub option_id: Option<String>,
    pub answer_ref: Option<ArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionRequestGetParams {
    pub request_id: Id,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum MissionRequestState {
    NotFound,
    Committed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionRequestGetResult {
    pub state: MissionRequestState,
    pub result: Option<super::types::MutationResult>,
}

// A user's confirmation that a run's process is gone although the daemon has
// no termination evidence. Provider outcome and external effects stay unknown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionRunAttestExitedParams {
    pub request_id: Id,
    pub mission_id: Id,
    pub expected_revision: U64String,
    pub run_id: Id,
    // Key of the statement the user confirmed (audit record only).
    pub attestation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionActivityParams {
    pub mission_id: Id,
    pub run_id: Id,
    pub after_offset: U64String,
    pub max_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MissionActivityResult {
    pub body_ref: Option<ArtifactRef>,
    pub next_offset: U64String,
    pub complete: bool,
}

// ---- binding / template / verification --------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BindingListResult {
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct BindingSaveParams {
    pub request_id: Id,
    /// 0 creates; otherwise CAS against the stored revision.
    pub expected_revision: U64String,
    pub binding: Binding,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct BindingSaveResult {
    pub binding: Binding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct BindingProbeParams {
    pub binding_id: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ProbeModel {
    pub id: String,
    pub efforts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct BindingProbeResult {
    pub binding: Binding,
    pub models: Vec<ProbeModel>,
    pub installation: InstallationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum InstallationStatus {
    Verified,
    NotFound,
    Failed,
    TimedOut,
    OutputLimit,
    UnrecognizedVersion,
}

// `runtime.detect` takes empty params (like `binding.list`) and stores nothing.
// Plain `//` comments on purpose: ts-rs copies `///` docs into src/generated.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum LoginHint {
    Found,
    NotFound,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityGrade {
    // Shipped evidence (exact version or `version_line`) passes the four
    // setup roles without consent.
    Verified,
    // No shipped evidence, but this machine's own probe / observed runs pass
    // the four setup roles without consent (11 §3.2).
    VerifiedLocally,
    // Shipped evidence exists on this version line, but the local protocol
    // self-check has not run (or failed) for this version.
    SameLineUnverified,
    Unverified,
    NotInstalled,
}

/// Model candidates for a second provider route one detected CLI serves. Claude
/// Code is the case today: the same executable routes to Z.ai Coding Plan
/// (`claude-exec --provider zai`, the `ccg` launch profile), so its detection
/// row can carry that route's observed ids beside the Anthropic ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct RuntimeAltModels {
    pub provider_id: String,
    pub models: Vec<ProbeModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct DetectedRuntime {
    pub runtime: RuntimeKind,
    // Absolute executable path; empty when no candidate was found.
    pub program: String,
    pub version: Option<String>,
    pub installation: InstallationStatus,
    // Presence of a local login marker only; never credential contents.
    pub login: LoginHint,
    pub configured_model_id: Option<String>,
    pub suggested_provider_id: String,
    // Set only when live evidence for this OS, architecture, and detected
    // version pins one model.
    pub proven_model_id: Option<String>,
    // Models this runtime actually offers right now, best effort: the live
    // listing when the adapter has one (Codex `model/list`), otherwise the
    // ids the local CLI has been observed using. Empty when nothing could be
    // read; it is a picker hint, never evidence, and never gates a role.
    pub models: Vec<ProbeModel>,
    // Candidates for a second provider route this CLI serves (see
    // [`RuntimeAltModels`]); absent when there is none. A picker hint, never
    // evidence, exactly like `models`.
    // `Option`: ts-rs의 `#[ts(optional)]`은 Option 필드에만 붙는다 — 비어 있으면
    // `None`이고 직렬화에서 빠져 TS 쪽은 `alt_models?: RuntimeAltModels[]`가 된다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub alt_models: Option<Vec<RuntimeAltModels>>,
    // Roles whose start-gate capability check a subscription binding built
    // from this row passes; empty when no model is known (start rejects it).
    pub verified_roles: Vec<Role>,
    pub grade: CompatibilityGrade,
    // Roles the same binding passes after the user accepts experimental use
    // of exactly this version; adapter-unimplemented capabilities stay blocked.
    pub experimental_roles: Vec<Role>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDetectResult {
    // Always codex, claude, opencode in this order.
    pub runtimes: Vec<DetectedRuntime>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct TemplateListParams {
    pub repository_id: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct TemplateListResult {
    pub templates: Vec<TeamTemplate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct TemplateSaveParams {
    pub request_id: Id,
    pub expected_revision: U64String,
    pub template: TeamTemplate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct TemplateSaveResult {
    pub template: TeamTemplate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct VerificationListParams {
    pub repository_id: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct VerificationListResult {
    pub commands: Vec<VerificationCommand>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct VerificationSaveParams {
    pub request_id: Id,
    pub expected_revision: U64String,
    pub command: VerificationCommand,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct VerificationSaveResult {
    pub command: VerificationCommand,
}

// ---- workspace cleanup --------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceUsageParams {
    // null = every mission with daemon-owned workspaces still on disk.
    pub mission_id: Option<Id>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceUsageEntry {
    pub mission_id: Id,
    pub workspaces: u32,
    // Bounded measurement: a lower bound when the size walk hit its limit.
    pub bytes: U64String,
    pub cleanable: bool,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceUsageResult {
    pub missions: Vec<WorkspaceUsageEntry>,
    pub total_bytes: U64String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCleanupParams {
    pub request_id: Id,
    pub mission_id: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceKept {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCleanupResult {
    pub removed: u32,
    pub freed_bytes: U64String,
    pub kept: Vec<WorkspaceKept>,
}

// ---- artifact transfer -------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ArtifactBeginParams {
    pub request_id: Id,
    /// Null = staging before mission.create (bound to the creating client).
    pub mission_id: Option<Id>,
    pub media_type: String,
    pub bytes: U64String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ArtifactBeginResult {
    pub upload_id: Id,
    pub chunk_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ArtifactWriteParams {
    pub upload_id: Id,
    pub offset: U64String,
    pub data_b64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ArtifactWriteResult {
    pub next_offset: U64String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ArtifactCommitParams {
    pub upload_id: Id,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReadParams {
    pub artifact_id: Id,
    pub offset: U64String,
    pub max_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReadResult {
    pub data_b64: String,
    pub next_offset: U64String,
    pub complete: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_reject_unknown_fields() {
        let base = serde_json::json!({
            "cursor": null, "limit": 10, "archived": false, "extra": true
        });
        assert!(serde_json::from_value::<MissionListParams>(base).is_err());
        let ok = serde_json::json!({"cursor": null, "limit": 10, "archived": false});
        let params: MissionListParams = serde_json::from_value(ok).unwrap();
        assert_eq!(params.limit, 10);
    }

    #[test]
    fn method_constants_are_dotted_paths() {
        assert_eq!(methods::MISSION_PLAN_APPLY, "mission.plan.apply");
        assert_eq!(methods::ARTIFACT_BEGIN, "artifact.begin");
        assert_eq!(methods::RUNTIME_DETECT, "runtime.detect");
        assert_eq!(
            methods::MISSION_RUN_ATTEST_EXITED,
            "mission.run.attest_exited"
        );
        assert_eq!(methods::WORKSPACE_USAGE, "workspace.usage");
        assert_eq!(methods::WORKSPACE_CLEANUP, "workspace.cleanup");
        assert_eq!(EVENT_MISSION_CHANGED, "mission.changed");
    }
}
