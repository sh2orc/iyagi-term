/** O1 reference wire contract. Runtime source of truth after O02: Rust + ts-rs.
 * No imports, credentials, provider SDK objects, or executable implementation.
 * null means unknown/not applicable; absent fields are not implicit defaults.
 */
export type Id = string; // UUID v4, validated in Rust
export type U64 = string; // decimal 0..9223372036854775807
export type Timestamp = string; // UTC RFC3339
export type Sha256 = string; // 64 lowercase hex
export type GitOid = string; // repository object format: 40 or 64 hex
export type MissionState = 'draft' | 'running' | 'pausing' | 'paused' | 'stopping' | 'completed' | 'failed' | 'cancelled';
export type TaskState = 'planned' | 'ready' | 'running' | 'awaiting_input' | 'awaiting_review' | 'blocked' | 'succeeded' | 'failed' | 'cancelled' | 'superseded';
export type RunState = 'prepared' | 'starting' | 'running' | 'awaiting_input' | 'stopping' | 'succeeded' | 'failed' | 'cancelled' | 'interrupted' | 'unknown';
export type Phase = 'planning' | 'implementing' | 'integrating' | 'validating' | 'reviewing' | 'awaiting_acceptance' | 'done';
export type Role = 'lead' | 'researcher' | 'architect' | 'builder' | 'test_author' | 'reviewer' | 'specialist' | 'diagnostician' | 'integrator' | 'documenter';
export type RuntimeKind = 'codex' | 'claude' | 'opencode' | 'fake';
export type TaskKind = 'plan' | 'research' | 'design' | 'implement' | 'test_author' | 'review' | 'consult' | 'diagnose' | 'integrate' | 'document' | 'verify';
export type AuthRoute = 'subscription' | 'api_key' | 'local' | 'custom';
// 11 §3.2; verified_locally (O22) is evidence-free local_probe/observed_runs passing all four roles without shipped fixtures.
export type CompatibilityGrade = 'verified' | 'verified_locally' | 'same_line_unverified' | 'unverified' | 'not_installed';
// reason_code slugs (11 §3.1): null (shipped evidence) | "version_line" | "local_probe" | "observed_runs" |
// "experimental_opt_in" | "local_probe_failed" | "adapter_not_implemented" | "no_compatibility_evidence" |
// other unproven shipped-fixture slugs (e.g. "print_mode_no_approval_reply").
export type Support = { supported: boolean; reason_code: string | null };
// Rust must reuse term_contracts::launch::LaunchPolicy, not define a second one.
export interface ResourcePolicy {
  reservation_bytes: U64; cpu_slots: number;
  enforcement: 'observe' | 'prefer' | 'require';
  memory_max_bytes: U64 | null; cpu_max_cores: number | null; pids_max: number | null;
}
export interface RuntimeCapabilities {
  structured_result: Support;
  events: Support;
  cancel: Support;
  resume: Support;
  steer: Support;
  approval_reply: Support;
  read_only: Support;
  scoped_write: Support;
  model_listing: Support;
  usage: Support;
  native_terminal_attach: Support;
}
export interface Binding {
  id: Id;
  revision: U64;
  label: string;
  runtime: RuntimeKind;
  program: string; // absolute executable path, never shell text
  runtime_version: string | null;
  provider_id: string;
  model_id: string;
  effort: string | null;
  auth_route: AuthRoute;
  credential_ref: string | null; // opaque secret store reference
  endpoint_ref: Id | null; // daemon-owned validated endpoint config
  capabilities: RuntimeCapabilities;
  checked_at: Timestamp | null;
  enabled: boolean;
  estimated_run_cost_usd_micros?: U64 | null; // user estimate; admission reservation, not a hard cap
  resource_policy: ResourcePolicy;
  // CLI version the user accepted experimental automated use for; no longer required to equal the observed version (11 §3.4).
  experimental_version?: string | null;
  // Daemon-owned local evidence (11 §3.3). Client-sent values on binding.save are discarded; omitted when None.
  local_evidence?: LocalEvidence | null;
}
// 11 §3.3. Daemon-measured compatibility evidence for one binding on this machine.
// Written only by the daemon (binding.probe, successful Runs); applies only while os/version equal the
// current installation observation.
export interface LocalEvidence {
  os: string;
  version: string;
  model_id: string; // model the run counters were collected for; probe results are model-independent
  probed_at: Timestamp | null;
  probe: LocalProbeReport | null; // what the no-inference self-check proved; null when it has not run for this version
  runs: LocalRunEvidence;
}
export interface LocalProbeReport {
  protocol_ok: boolean; // the CLI completed the adapter's protocol handshake / advertises every flag the adapter passes
  sandbox_cases_passed: number | null; // OS-level write/network boundary cases (Codex command/exec); null when not attempted
  sandbox_cases_total: number | null;
  model_listed: boolean | null; // the binding's model appeared in the runtime's own listing; null when the runtime has none
  failures: string[]; // short fixed slugs of failed checks, never raw CLI output
}
export interface LocalRunEvidence {
  succeeded_read_only: number;
  succeeded_write: number;
  cancelled: number;
  invalid_result: number;
  last_at: Timestamp | null;
}
export interface RoleBinding {
  role: Role;
  primary_binding_id: Id;
  fallback_binding_ids: Id[]; // ordered; user-configured; never inferred
}
export interface Policy {
  max_parallel_runs: number;
  max_attempts_per_task: number;
  max_repair_cycles: number;
  max_automatic_starts: number;
  active_time_limit_ms: number;
  run_time_limit_ms: number;
  max_cost_usd_micros: U64 | null;
  unknown_cost: 'allow_with_notice' | 'block';
  allow_network: boolean;
  allow_automatic_plan_apply: boolean;
  allow_recovery_of_unsent: boolean;
  allowed_binding_ids: Id[];
  allowed_roles: Role[];
  allowed_verification_ids: Id[];
  require_independent_review: boolean;
  require_enforced_verification: boolean;
}
export interface ArtifactRef { id: Id; sha256: Sha256; bytes: U64; media_type: string }
export interface Requirement { id: Id; text: string; verification_ids: Id[]; human_check: boolean }
export interface Mission {
  id: Id; revision: U64; semantic_revision?: U64 | null; state: MissionState; phase: Phase;
  title: string; repository_path: string; repository_id: Id; base_oid: GitOid;
  goal_ref: ArtifactRef; requirements: Requirement[]; policy: Policy;
  role_bindings: RoleBinding[]; plan_revision: number;
  candidate_id: Id | null; open_decision_count: number;
  active_time_ms: U64; automatic_start_count: number;
  created_at: Timestamp; updated_at: Timestamp; archived_at: Timestamp | null;
  accepted_at: Timestamp | null; failure_code: ErrorCode | null;
  follow_up_of?: Id | null; // accepted mission whose candidate commit is base_oid
}
export interface TaskContract {
  objective_ref: ArtifactRef;
  requirement_ids: Id[];
  input_artifact_ids: Id[];
  allowed_paths: string[]; // exact relative path or trailing / directory prefix
  expected_outputs: ('report' | 'patch' | 'review' | 'verification')[];
  verification_ids: Id[];
  specialty: string | null;
}
export interface IntegrationTask {
  plan_ref: ArtifactRef;
  step: 'automatic' | { resolving: { conflict_run_id: Id } }
    | { continuing: { conflict_run_id: Id; resolution_run_id: Id } };
}
export interface Task {
  id: Id; mission_id: Id; title: string; kind: TaskKind; role: Role | null;
  state: TaskState; required: boolean; parent_task_id: Id | null;
  depends_on: Id[]; contract: TaskContract; binding_id: Id | null;
  active_run_id: Id | null; ordinal: number; attempt_count: number;
  repair_cycle: number; replacement_of: Id | null;
  integration?: IntegrationTask | null; // absent legacy records retain their original mode
  failure_repair_run_ids?: Id[]; // daemon-owned exact failed Runs; absent legacy records mean []
  blocked_code: string | null; workspace_id: Id | null;
  dispatch_after_unix_ms?: U64 | null;
  created_at: Timestamp; updated_at: Timestamp;
}
export interface Usage {
  input_tokens: U64 | null; output_tokens: U64 | null;
  cost_usd_micros: U64 | null; cost_source: 'provider' | 'estimate' | 'unknown';
}
export interface Run {
  id: Id; mission_id: Id; task_id: Id; attempt: number; state: RunState;
  binding_snapshot: Binding | null; // null for deterministic verification/integration
  requested_model: string | null; observed_model: string | null;
  provider_session_id: string | null; provider_turn_id: string | null;
  exec_id: Id | null; pty_session_id: Id | null; workspace_id: Id | null;
  fencing_token: U64; dispatch_state: 'unsent' | 'may_have_sent' | 'acknowledged';
  // Provider context, or the daemon-frozen command/candidate/workspace contract for a supervised Verify Run.
  context_ref: ArtifactRef; result_ref: ArtifactRef | null;
  usage: Usage; last_activity_at: Timestamp | null;
  active_time_ms: U64;
  started_at: Timestamp | null; ended_at: Timestamp | null;
  failure_code: ErrorCode | null;
  reconciliation_ref: ArtifactRef | null;
  rate_limit?: RateLimitObservation | null;
  retry_evidence?: RetryEvidence | null;
  // What reconciliation_ref proves: observed durable exit or only the user's attestation.
  reconciliation_kind?: 'exec_exited' | 'user_attested' | null;
}
export type RetryEvidence = {
  basis: 'request_not_submitted';
  observed_at_unix_ms: U64;
  retry_after_unix_ms: U64 | null;
} | {
  basis: 'plan_format_rejected';
  plan_revision: number;
  rejected_result_ref: ArtifactRef | null;
};
export interface RateLimitObservation {
  observed_at_unix_ms: U64;
  resets_at_unix_ms: U64;
}
export interface Message {
  id: Id; mission_id: Id; target_task_id: Id | null;
  role: 'user' | 'agent' | 'system'; run_id: Id | null;
  body_ref: ArtifactRef;
  delivery: 'queued' | 'delivered' | 'rejected' | 'unknown';
  supersedes_message_id: Id | null; created_at: Timestamp;
}
export interface ExecRecord {
  id: Id; mission_id: Id; run_id: Id;
  state: 'prepared' | 'spawned' | 'stopping' | 'exited' | 'unknown';
  identity: { pid: number; start_token: string; boot_id: string } | null;
  group_kind: 'cgroup' | 'job' | 'observed_tree' | null;
  group_reference: string | null;
  group_identity: { kind: 'cgroup_v2'; boot_id: string; kernel_id: string }
    | { kind: 'macos_guardian'; guardian: { pid: number; start_token: string; boot_id: string }; endpoint: string } | null;
  resource_policy: ResourcePolicy;
  launch_manifest_ref: ArtifactRef; // sanitized program/argv, no secret env values
  owner_daemon_id: Id; started_at: Timestamp | null; ended_at: Timestamp | null;
  exit_code: number | null;
}
export interface Decision {
  id: Id; mission_id: Id; requesting_run_id: Id | null;
  kind: 'product' | 'approval' | 'budget' | 'recovery' | 'plan' | 'conflict';
  state: 'open' | 'answered' | 'obsolete';
  question_ref: ArtifactRef; options: { id: string; label: string }[];
  affected_task_ids: Id[]; blocking: boolean;
  plan_revision: number; candidate_id: Id | null;
  answer_ref: ArtifactRef | null; selected_option_id: string | null;
  answer_message_id: Id | null;
  created_at: Timestamp; answered_at: Timestamp | null;
}
export interface Workspace {
  id: Id; mission_id: Id; path: string;
  kind: 'worker' | 'integration' | 'verification' | 'experiment';
  base_oid: GitOid; head_oid: GitOid;
  writer_run_id: Id | null; lease_token: U64;
  state: 'preparing' | 'ready' | 'busy' | 'quarantined' | 'retained';
  owned_by_daemon: boolean;
}
export interface Candidate {
  id: Id; mission_id: Id; revision: number; base_oid: GitOid; tree_oid: GitOid;
  commit_oid: GitOid; source_run_ids: Id[]; manifest_ref: ArtifactRef;
  created_at: Timestamp; supersedes_id: Id | null;
}
export interface VerificationCommand {
  id: Id; title: string; program: string; argv: string[];
  revision: U64; repository_id: Id;
  cwd_relative: string; timeout_ms: number;
  env_profile_ref: Id | null; allowed_network: boolean;
}
export interface Verification {
  id: Id; mission_id: Id; candidate_id: Id; task_id: Id; run_id: Id;
  command_snapshot_ref: ArtifactRef; environment_ref: ArtifactRef;
  requirement_ids: Id[];
  status: 'passed' | 'failed' | 'cancelled' | 'unknown';
  input_integrity: 'enforced' | 'observed' | 'unknown';
  exit_code: number | null; log_ref: ArtifactRef;
  started_at: Timestamp; ended_at: Timestamp;
}
export interface Finding {
  id: Id; mission_id: Id; candidate_id: Id; reviewer_run_id: Id;
  severity: 'blocking' | 'major' | 'minor' | 'note';
  path: string | null; line: number | null;
  evidence_ref: ArtifactRef; requirement_id: Id | null;
  resolution: 'open' | 'fixed' | 'dismissed'; resolution_ref: ArtifactRef | null;
}
export interface Knowledge {
  id: Id; mission_id: Id; kind: 'fact' | 'hypothesis' | 'decision' | 'question';
  body_ref: ArtifactRef; source_artifact_ids: Id[]; source_run_id: Id | null;
  base_oid: GitOid; related_paths: string[];
  status: 'proposed' | 'accepted' | 'stale' | 'rejected';
  supersedes_id: Id | null;
}
export interface ContextBundle {
  version: 1; mission_id: Id; task_id: Id; base_oid: GitOid;
  goal_ref: ArtifactRef; contract: TaskContract;
  artifacts: ArtifactRef[]; knowledge_ids: Id[];
  prior_run_ids: Id[]; omitted: { artifact_id: Id; reason: string }[];
}
export interface PlanProposal {
  id: Id; mission_id: Id; based_on_plan_revision: number;
  tasks: TaskSpec[]; retire_task_ids: Id[];
  rationale_ref: ArtifactRef;
}
export interface TeamTemplate {
  id: Id; revision: U64; label: string;
  repository_id: Id | null; role_bindings: RoleBinding[]; policy: Policy;
}
/** Provider receives these body fields, never fabricated ArtifactRef IDs.
 * local_key is ASCII [a-z][a-z0-9_-]{0,63}; daemon allocates UUID once.
 */
export interface ProviderTaskSpec {
  local_key: string; title: string; kind: TaskKind; role: Role | null;
  required: boolean; parent_key: string | null; depends_on_keys: string[];
  objective_text: string; requirement_ids: Id[]; input_artifact_ids: Id[];
  allowed_paths: string[]; expected_outputs: TaskContract['expected_outputs'];
  verification_ids: Id[]; specialty: string | null; binding_id: Id | null;
  replacement_of: Id | null;
}
export type ProviderResult =
  | { kind: 'plan'; based_on_plan_revision: number; tasks: ProviderTaskSpec[]; retire_task_ids: Id[]; rationale_text: string }
  | { kind: 'report'; report_text: string; knowledge: { kind: Knowledge['kind']; text: string; source_artifact_ids: Id[]; related_paths: string[] }[] }
  | { kind: 'patch'; report_text: string; verification_claims: string[] }
  | { kind: 'review'; candidate_id: Id; report_text: string; findings: { severity: Finding['severity']; path: string | null; line: number | null; evidence_text: string; requirement_id: Id | null }[] }
  | { kind: 'question'; question_text: string; options: { id: string; label: string }[] }
  | { kind: 'blocked'; code: string; report_text: string };
export interface TaskSpec {
  id: Id; title: string; kind: TaskKind; role: Role | null;
  required: boolean; parent_task_id: Id | null;
  depends_on: Id[]; contract: TaskContract; binding_id: Id | null;
  replacement_of: Id | null;
}
export type AgentResult =
  | { kind: 'plan'; proposal: PlanProposal }
  | { kind: 'report'; report_ref: ArtifactRef; knowledge: Knowledge[] }
  | { kind: 'patch'; report_ref: ArtifactRef; verification_claims: string[] }
  | { kind: 'review'; candidate_id: Id; findings: Finding[]; report_ref: ArtifactRef }
  | { kind: 'question'; question_ref: ArtifactRef; options: { id: string; label: string }[] }
  | { kind: 'blocked'; code: string; report_ref: ArtifactRef };
// patch result is a claim. Daemon derives actual changed files/candidate itself.
export type Entity =
  | { kind: 'mission'; value: Mission } | { kind: 'task'; value: Task }
  | { kind: 'run'; value: Run } | { kind: 'message'; value: Message }
  | { kind: 'decision'; value: Decision } | { kind: 'workspace'; value: Workspace }
  | { kind: 'candidate'; value: Candidate } | { kind: 'verification'; value: Verification }
  | { kind: 'finding'; value: Finding } | { kind: 'knowledge'; value: Knowledge }
  | { kind: 'exec'; value: ExecRecord };
export interface Change {
  entity_kind: Entity['kind']; entity_id: Id;
  operation: 'upsert' | 'delete'; // delete only expired content projections, not history
}
export interface MissionEvent {
  mission_id: Id; seq: U64; revision: U64; transaction_id: Id;
  type: 'created' | 'changed' | 'plan_applied' | 'decision_answered' | 'run_dispatched' | 'reconciled' | 'accepted' | 'archived';
  changes: Change[] | null; changes_ref: ArtifactRef | null; created_at: Timestamp;
}
export interface SnapshotPage {
  snapshot_id: Id; mission_id: Id; at_seq: U64; revision: U64;
  entities: Entity[]; next_cursor: string | null; expires_at: Timestamp;
}
export interface Mutation { request_id: Id; mission_id: Id; expected_revision: U64 }
export interface MutationResult { mission_id: Id; revision: U64; event_seq: U64; entity_ids: Id[] }
export type ErrorCode =
  | 'INVALID_ARGUMENT' | 'INVALID_STATE' | 'NOT_FOUND' | 'REQUEST_CONFLICT'
  | 'REVISION_CONFLICT' | 'CAPABILITY_UNSUPPORTED' | 'MODEL_UNAVAILABLE'
  | 'AUTH_REQUIRED' | 'PROVIDER_RATE_LIMITED' | 'PROVIDER_UNAVAILABLE'
  | 'BUDGET_EXCEEDED' | 'UNKNOWN_COST' | 'POLICY_DENIED' | 'DIRTY_WORKTREE'
  | 'WORKSPACE_BUSY' | 'PLAN_CYCLE' | 'PLAN_LIMIT' | 'CONTEXT_TOO_LARGE'
  | 'STALE_DECISION' | 'STALE_CANDIDATE' | 'RESULT_INVALID' | 'OUTCOME_UNKNOWN'
  | 'CONTENT_EXPIRED' | 'SNAPSHOT_EXPIRED' | 'CURSOR_EXPIRED'
  | 'ARTIFACT_LIMIT' | 'INTEGRITY_FAILED' | 'STORAGE_UNAVAILABLE' | 'INTERNAL';
export interface RpcError {
  code: ErrorCode; message: string; retryable: boolean;
  details: { current_revision?: U64; reason_code?: string; retry_after_ms?: number; decision_id?: Id };
}
export interface Rpc {
  'mission.create': { params: {
    request_id: Id; title: string; repository_path: string; expected_base_oid: GitOid;
    goal_ref: ArtifactRef; requirements: Requirement[]; policy: Policy; role_bindings: RoleBinding[];
    follow_up_of?: Id | null; // completed mission; expected_base_oid = its accepted candidate commit
  }; result: MutationResult };
  'mission.list': { params: { cursor: string | null; limit: number; archived: boolean }; result: { items: Mission[]; next_cursor: string | null } };
  'mission.snapshot': { params: { mission_id: Id; snapshot_id: Id | null; cursor: string | null }; result: SnapshotPage };
  'mission.events': { params: { mission_id: Id; after_seq: U64; limit: number }; result: { events: MissionEvent[]; high_watermark: U64; next_after_seq: U64 } };
  'mission.control': { params: Mutation & { action: 'start' | 'pause' | 'resume' | 'cancel' | 'archive' | 'unarchive' }; result: MutationResult };
  'mission.accept': { params: Mutation & { candidate_id: Id; acknowledged_verification_ids: Id[]; human_requirement_ids: Id[]; acknowledged_reconciled_run_ids?: Id[] | null }; result: MutationResult };
  'mission.message': { params: Mutation & { target_task_id: Id | null; body_ref: ArtifactRef; supersedes_message_id?: Id | null }; result: MutationResult };
  'mission.plan.apply': { params: Mutation & { proposal_ref: ArtifactRef }; result: MutationResult };
  // retry + binding_id atomically changes the binding and authorizes a new attempt.
  'mission.task.control': { params: Mutation & { task_id: Id; action: 'cancel' | 'retry' | 'reassign'; binding_id: Id | null }; result: MutationResult };
  'mission.policy.update': { params: Mutation & { policy: Policy; role_bindings: RoleBinding[] }; result: MutationResult };
  'mission.finding.resolve': { params: Mutation & { finding_id: Id; resolution: 'dismissed'; reason_ref: ArtifactRef }; result: MutationResult };
  'mission.decision.answer': { params: Mutation & { decision_id: Id; option_id: string | null; answer_ref: ArtifactRef | null }; result: MutationResult };
  'mission.request.get': { params: { request_id: Id }; result: { state: 'not_found' | 'committed'; result: MutationResult | null } };
  'mission.activity': { params: { mission_id: Id; run_id: Id; after_offset: U64; max_bytes: number }; result: { body_ref: ArtifactRef | null; next_offset: U64; complete: boolean } };
  // User confirms an unknown/interrupted run's process is gone without daemon evidence; outcome/effects stay unknown.
  'mission.run.attest_exited': { params: Mutation & { run_id: Id; attestation: string }; result: MutationResult };
  // Bounded size estimate of daemon-owned worktrees still on disk; mission_id null = all with workspaces.
  'workspace.usage': { params: { mission_id: Id | null }; result: { missions: { mission_id: Id; workspaces: number; bytes: U64; cleanable: boolean; blocked_reason: string | null }[]; total_bytes: U64 } };
  // User-requested removal for terminal missions; dirty/unowned/active workspaces are kept with a reason.
  'workspace.cleanup': { params: { request_id: Id; mission_id: Id }; result: { removed: number; freed_bytes: U64; kept: { path: string; reason: string }[] } };
  'binding.list': { params: Record<string, never>; result: { bindings: Binding[] } };
  'binding.save': { params: { request_id: Id; expected_revision: U64; binding: Binding }; result: { binding: Binding } };
  'binding.probe': { params: { binding_id: Id }; result: { binding: Binding; models: { id: string; efforts: string[] }[]; installation: 'verified' | 'not_found' | 'failed' | 'timed_out' | 'output_limit' | 'unrecognized_version' } };
  // Read-only first-run discovery; stores nothing, never returns credential contents. Always codex, claude, opencode.
  'runtime.detect': { params: Record<string, never>; result: { runtimes: { runtime: RuntimeKind; program: string; version: string | null; installation: 'verified' | 'not_found' | 'failed' | 'timed_out' | 'output_limit' | 'unrecognized_version'; login: 'found' | 'not_found' | 'unknown'; configured_model_id: string | null; suggested_provider_id: string; proven_model_id: string | null; models: { id: string; efforts: string[] }[]; verified_roles: Role[]; grade: CompatibilityGrade; experimental_roles: Role[] }[] } };
  'repository.inspect': { params: { path: string }; result: { repository_id: Id; canonical_path: string; head_oid: GitOid; clean: boolean; dirty_paths: string[]; verification_supported: boolean } };
  'template.list': { params: { repository_id: Id | null }; result: { templates: TeamTemplate[] } };
  'template.save': { params: { request_id: Id; expected_revision: U64; template: TeamTemplate }; result: { template: TeamTemplate } };
  'verification.list': { params: { repository_id: Id }; result: { commands: VerificationCommand[] } };
  'verification.save': { params: { request_id: Id; expected_revision: U64; command: VerificationCommand }; result: { command: VerificationCommand } };
  'artifact.begin': { params: { request_id: Id; mission_id: Id | null; media_type: string; bytes: U64; sha256: Sha256 }; result: { upload_id: Id; chunk_bytes: number } };
  'artifact.write': { params: { upload_id: Id; offset: U64; data_b64: string }; result: { next_offset: U64 } };
  'artifact.commit': { params: { upload_id: Id }; result: ArtifactRef };
  'artifact.read': { params: { artifact_id: Id; offset: U64; max_bytes: number }; result: { data_b64: string; next_offset: U64; complete: boolean } };
}
