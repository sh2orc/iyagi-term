//! # O1 mission orchestration contracts
//!
//! Wire DTOs, RPC vocabularies, state-transition tables, and pure validators
//! mirroring `docs/orchestration/contracts.ts` (ticket O02). This module is
//! data + validation only: no I/O, no clock, no DB — the engine (term-core)
//! and daemon (iyagi-termd) build on it. R1 terminal contracts are untouched;
//! `MissionState` and `WorkloadState` are deliberately distinct types.

pub mod error;
pub mod plan;
pub mod rpc;
pub mod states;
pub mod types;
pub mod validation;

pub use error::{MissionErrorCode, MissionErrorDetails, MissionRpcError};
pub use plan::{validate_plan_graph, PlanGraphError, PlanNode};
pub use types::{
    AgentResult, ArtifactRef, Binding, Candidate, Change, ChangeOperation, ContextBundle,
    ContextOmission, Decision, DecisionKind, DecisionOption, DecisionState, Entity, EntityKind,
    ExecGroupKind, ExecRecord, ExecState, ExpectedOutput, Finding, FindingResolution,
    FindingSeverity, GitOid, Id, InputIntegrity, Knowledge, KnowledgeKind, KnowledgeStatus,
    Message, MessageDelivery, MessageRole, Mission, MissionEvent, MissionEventType, MissionState,
    Phase, PlanProposal, Policy, ProviderFindingDraft, ProviderKnowledgeDraft, ProviderResult,
    ProviderTaskSpec, Requirement, Role, RoleBinding, Run, RunDispatchState, RunState,
    RuntimeCapabilities, RuntimeKind, Sha256, SnapshotPage, Support, Task, TaskContract, TaskKind,
    TaskSpec, TaskState, TeamTemplate, Timestamp, UnknownCostPolicy, Usage, UsageCostSource,
    Verification, VerificationCommand, VerificationStatus, Workspace, WorkspaceKind,
    WorkspaceState,
};
pub use validation::{
    default_role_for_kind, ensure_all_in_scope, expected_outputs_for_kind, has_no_control_chars,
    result_variant_matches_task, role_matches_kind, validate_allowed_path, validate_artifact_ref,
    validate_git_oid, validate_local_key, validate_policy, validate_provider_session_id,
    validate_requirements, validate_role_bindings, validate_sha256, validate_task_contract,
    validate_title, MissionLimits, PolicyCeiling, ValidationResult,
};
