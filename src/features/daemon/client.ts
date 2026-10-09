import type { RepositoryInspectParams } from "../../generated/RepositoryInspectParams";
import type { RepositoryInspectResult } from "../../generated/RepositoryInspectResult";
/**
 * Typed, mockable daemon client surface (01-contracts.md §4).
 *
 * The real transport (UDS/named-pipe frames via the Tauri bridge) arrives in
 * a later ticket; every consumer depends on this interface only, so
 * `MockDaemonClient` drives the same code paths headlessly.
 */

import type { AgentSessionForgetParams } from "../../generated/AgentSessionForgetParams";
import type { AgentSessionForgetResult } from "../../generated/AgentSessionForgetResult";
import type { AgentSessionListParams } from "../../generated/AgentSessionListParams";
import type { AgentSessionRecord } from "../../generated/AgentSessionRecord";
import type { ArtifactBeginParams } from "../../generated/ArtifactBeginParams";
import type { ArtifactBeginResult } from "../../generated/ArtifactBeginResult";
import type { ArtifactCommitParams } from "../../generated/ArtifactCommitParams";
import type { ArtifactReadParams } from "../../generated/ArtifactReadParams";
import type { ArtifactReadResult } from "../../generated/ArtifactReadResult";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import type { ArtifactWriteParams } from "../../generated/ArtifactWriteParams";
import type { ArtifactWriteResult } from "../../generated/ArtifactWriteResult";
import type { AttachParams } from "../../generated/AttachParams";
import type { AttachResult } from "../../generated/AttachResult";
import type { BindingListResult } from "../../generated/BindingListResult";
import type { BindingProbeParams } from "../../generated/BindingProbeParams";
import type { BindingProbeResult } from "../../generated/BindingProbeResult";
import type { BindingSaveParams } from "../../generated/BindingSaveParams";
import type { BindingSaveResult } from "../../generated/BindingSaveResult";
import type { ErrorCode } from "../../generated/ErrorCode";
import type { HostSample } from "../../generated/HostSample";
import type { InputParams } from "../../generated/InputParams";
import type { InputResult } from "../../generated/InputResult";
import type { LaunchPolicy } from "../../generated/LaunchPolicy";
import type { LaunchRequest } from "../../generated/LaunchRequest";
import type { Metric } from "../../generated/Metric";
import type { MissionAcceptParams } from "../../generated/MissionAcceptParams";
import type { MissionActivityParams } from "../../generated/MissionActivityParams";
import type { MissionActivityResult } from "../../generated/MissionActivityResult";
import type { MissionControlParams } from "../../generated/MissionControlParams";
import type { MissionCreateParams } from "../../generated/MissionCreateParams";
import type { MissionDecisionAnswerParams } from "../../generated/MissionDecisionAnswerParams";
import type { MissionErrorCode } from "../../generated/MissionErrorCode";
import type { MissionEventsParams } from "../../generated/MissionEventsParams";
import type { MissionEventsResult } from "../../generated/MissionEventsResult";
import type { MissionFindingResolveParams } from "../../generated/MissionFindingResolveParams";
import type { MissionListParams } from "../../generated/MissionListParams";
import type { MissionListResult } from "../../generated/MissionListResult";
import type { MissionMessageParams } from "../../generated/MissionMessageParams";
import type { MissionPlanApplyParams } from "../../generated/MissionPlanApplyParams";
import type { MissionPolicyUpdateParams } from "../../generated/MissionPolicyUpdateParams";
import type { MissionRequestGetParams } from "../../generated/MissionRequestGetParams";
import type { MissionRequestGetResult } from "../../generated/MissionRequestGetResult";
import type { MissionRunAttestExitedParams } from "../../generated/MissionRunAttestExitedParams";
import type { MissionSnapshotParams } from "../../generated/MissionSnapshotParams";
import type { MissionTaskControlParams } from "../../generated/MissionTaskControlParams";
import type { MutationResult } from "../../generated/MutationResult";
import type { Priority } from "../../generated/Priority";
import type { ProcessIdentity } from "../../generated/ProcessIdentity";
import type { QueueEntry } from "../../generated/QueueEntry";
import type { RequestId } from "../../generated/RequestId";
import type { RuntimeDetectResult } from "../../generated/RuntimeDetectResult";
import type { ResizeParams } from "../../generated/ResizeParams";
import type { ResizeResult } from "../../generated/ResizeResult";
import type { RpcEventKind } from "../../generated/RpcEventKind";
import type { SessionAck } from "../../generated/SessionAck";
import type { SessionExit } from "../../generated/SessionExit";
import type { InterventionNotice } from "../../generated/InterventionNotice";
import type { SessionFocusParams } from "../../generated/SessionFocusParams";
import type { SessionFocusResult } from "../../generated/SessionFocusResult";
import type { SessionReliefParams } from "../../generated/SessionReliefParams";
import type { SessionReliefResult } from "../../generated/SessionReliefResult";
import type { ReliefPolicy } from "../../generated/ReliefPolicy";
import type { ReliefPolicyParams } from "../../generated/ReliefPolicyParams";
import type { WorkloadSuspendParams } from "../../generated/WorkloadSuspendParams";
import type { WorkloadGuardResult } from "../../generated/WorkloadGuardResult";
import type { GuardPolicyParams } from "../../generated/GuardPolicyParams";
import type { GuardPolicy } from "../../generated/GuardPolicy";
import type { SessionSearchParams } from "../../generated/SessionSearchParams";
import type { SessionSearchResult } from "../../generated/SessionSearchResult";
import type { SessionId } from "../../generated/SessionId";
import type { SessionOutput } from "../../generated/SessionOutput";
import type { Snapshot } from "../../generated/Snapshot";
import type { SnapshotPage } from "../../generated/SnapshotPage";
import type { TemplateListParams } from "../../generated/TemplateListParams";
import type { TemplateListResult } from "../../generated/TemplateListResult";
import type { TemplateSaveParams } from "../../generated/TemplateSaveParams";
import type { TemplateSaveResult } from "../../generated/TemplateSaveResult";
import type { U64String } from "../../generated/U64String";
import type { VerificationListParams } from "../../generated/VerificationListParams";
import type { VerificationListResult } from "../../generated/VerificationListResult";
import type { VerificationSaveParams } from "../../generated/VerificationSaveParams";
import type { VerificationSaveResult } from "../../generated/VerificationSaveResult";
import type { ViewId } from "../../generated/ViewId";
import type { WorkloadId } from "../../generated/WorkloadId";
import type { WorkloadState } from "../../generated/WorkloadState";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import type { WorkspaceCleanupParams } from "../../generated/WorkspaceCleanupParams";
import type { WorkspaceCleanupResult } from "../../generated/WorkspaceCleanupResult";
import type { WorkspaceUsageParams } from "../../generated/WorkspaceUsageParams";
import type { WorkspaceUsageResult } from "../../generated/WorkspaceUsageResult";

/**
 * Structured error details (01-contracts §7). Every field is optional:
 * absent means genuinely unknown, never zero. Wire `null`s normalize to
 * missing fields.
 */
export interface RpcErrorDetails {
  /** Present on REVISION_CONFLICT: the mission's current revision. */
  current_revision?: string;
  /** Machine-readable reason slug (e.g. unsupported capability reason). */
  reason_code?: string;
  /** Present on PROVIDER_RATE_LIMITED when the reset window is known. */
  retry_after_ms?: number;
  /** Present on STALE_DECISION: the decision currently awaiting an answer. */
  decision_id?: string;
}

/**
 * RpcError shape from 01 §4 (`{code,message,retryable,details}`). Mission
 * RPCs reject with the O1 `MissionErrorCode` vocabulary, so `code` carries
 * either set (same JSON envelope, 01-contracts O1 §7).
 */
export class RpcClientError extends Error {
  readonly code: ErrorCode | MissionErrorCode;
  readonly retryable: boolean;
  readonly details?: RpcErrorDetails;

  constructor(
    code: ErrorCode | MissionErrorCode,
    message: string,
    retryable = false,
    details?: RpcErrorDetails,
  ) {
    super(message);
    this.name = "RpcClientError";
    this.code = code;
    this.retryable = retryable;
    this.details = details;
  }
}

export interface QueueChangedPayload {
  queue: QueueEntry[];
  revision: number;
}

export interface ResizeAppliedPayload {
  session_id: SessionId;
  epoch: string;
  seq: U64String;
  cols: number;
  rows: number;
}

export interface FlowBlockedPayload {
  session_id: SessionId;
  blocked: boolean;
  /** Writer view the daemon paused/resumed, when known. */
  view_id?: ViewId | null;
  unacked_bytes?: U64String | null;
}

export interface OwnerChangedPayload {
  session_id: SessionId;
  epoch: string;
  owner_view_id: ViewId | null;
}

export interface MissionChangedPayload {
  mission_id: string;
  /** Latest committed event seq (wire string — U64 never travels as JSON number). */
  latest_seq: U64String;
}

/**
 * 롤링 저널의 보존 창보다 뒤처진 view를 데몬이 떼어냈다(02-runner §5).
 * 그 view는 새 epoch로 다시 붙어야 하고, 재생은 `first_seq`부터다.
 */
export interface ReplayRequiredPayload {
  session_id: SessionId;
  view_id: ViewId;
  epoch: string;
  first_seq: U64String;
}

export type DaemonEvent = {
  // `mission.changed` is an O1 notification, not a member of the R1
  // RpcEventKind enum — the daemon emits it alongside the R1 event stream.
  readonly kind: RpcEventKind | "mission.changed";
} & (
  | { kind: "workload.changed"; payload: WorkloadSummary }
  | { kind: "queue.changed"; payload: QueueChangedPayload }
  | { kind: "resource.snapshot"; payload: HostSample }
  | { kind: "session.output"; payload: SessionOutput }
  | { kind: "session.resize_applied"; payload: ResizeAppliedPayload }
  | { kind: "session.flow_blocked"; payload: FlowBlockedPayload }
  | { kind: "session.exited"; payload: SessionExit }
  | { kind: "session.owner_changed"; payload: OwnerChangedPayload }
  | { kind: "session.replay_required"; payload: ReplayRequiredPayload }
  | { kind: "intervention.reported"; payload: InterventionNotice }
  | { kind: "mission.changed"; payload: MissionChangedPayload }
);

export type DaemonEventListener = (event: DaemonEvent) => void;

export interface EventSubscription {
  dispose(): void;
}

export interface DaemonEventStream {
  subscribe(listener: DaemonEventListener): EventSubscription;
}

export interface LaunchOutcome {
  workload_id: WorkloadId;
  /** null while a managed launch sits in the queue (no PTY yet — U14). */
  session_id: SessionId | null;
  state: WorkloadState;
  effective_policy: LaunchPolicy;
  missing_capabilities: string[];
}

export interface CancelParams {
  /** Immediately kill the owned process group instead of waiting for graceful exit. */
  force?: boolean;
  request_id: RequestId;
  workload_id: WorkloadId;
}

export interface CancelResult {
  state: WorkloadState;
}

export interface ReprioritizeParams {
  workload_id: WorkloadId;
  priority: Priority;
}

export interface ReprioritizeResult {
  queue: QueueEntry[];
}

export interface UpdatePolicyParams {
  workload_id: WorkloadId;
  policy: LaunchPolicy;
}

export interface UpdatePolicyResult {
  workload: WorkloadSummary;
}

export interface ProcessSummary {
  identity: ProcessIdentity;
  name: string;
  cpu_cores: Metric<number>;
  resident_bytes: Metric<U64String>;
}

export interface ProcessesParams {
  workload_id: WorkloadId;
  cursor: string | null;
  limit: number;
}

export interface ProcessesResult {
  processes: ProcessSummary[];
  next_cursor: string | null;
}

export interface DetachParams {
  session_id: SessionId;
  view_id: ViewId;
}

export interface DetachResult {
  detached: true;
}

export interface TakeControlParams {
  session_id: SessionId;
  view_id: ViewId;
  expected_owner: ViewId | null;
}

export interface TakeControlResult {
  epoch: string;
  owner_view_id: ViewId;
}

export interface RetentionSetLimitParams {
  session_id: SessionId;
  max_bytes: U64String;
}

export interface RetentionSetLimitResult {
  max_bytes: U64String;
}

/**
 * RPC methods from 01-contracts §4. `sessionAck` is fire-and-forget on the
 * data connection (정상 처리 시 별도 응답 없음).
 */
export interface DaemonClient {
  readonly events: DaemonEventStream;

  /**
   * Optional native-transport health seam. Browser mocks omit it; the Tauri
   * client exposes it so the session controller can recover a dead data pipe
   * even when the control RPC connection is still healthy.
   *
   * `daemonOutdated` is the build/version handshake result: the connected
   * daemon was built from a different source than this app (or is an OLD
   * daemon that predates the check). It never blocks the connection — it drives
   * the non-blocking "outdated — restart" banner.
   */
  transportStatus?(): Promise<{
    controlAlive: boolean;
    dataAlive: boolean;
    generation: number;
    daemonOutdated: boolean;
  }>;
  reconnectTransport?(): Promise<void>;
  /**
   * Retire the current (outdated) daemon without killing workloads
   * (`daemon.shutdown { stop_workloads: false }`), then reconnect so a fresh
   * daemon is spawned. Optional: browser mocks omit it.
   */
  restartDaemon?(): Promise<void>;
  /** Release frontend-only resources. Never terminates daemon workloads. */
  dispose?(): void;

  systemSnapshot(): Promise<Snapshot>;
  workloadLaunch(request: LaunchRequest): Promise<LaunchOutcome>;
  workloadCancel(params: CancelParams): Promise<CancelResult>;
  workloadReprioritize(params: ReprioritizeParams): Promise<ReprioritizeResult>;
  workloadUpdatePolicy(params: UpdatePolicyParams): Promise<UpdatePolicyResult>;
  workloadProcesses(params: ProcessesParams): Promise<ProcessesResult>;
  sessionAttach(params: AttachParams): Promise<AttachResult>;
  sessionDetach(params: DetachParams): Promise<DetachResult>;
  /**
   * Release the transport-side output route of one view (Tauri bridge:
   * drops the per-view channel so neither heap keeps it). Fire-and-forget;
   * mocks omit it. Called whenever a pipeline is thrown away — close,
   * retry, controller dispose — not on detach alone, since a detached view
   * may still be re-attached through the same pipeline.
   */
  sessionUnsubscribe?(params: DetachParams): void;
  sessionInput(params: InputParams): Promise<InputResult>;
  sessionResize(params: ResizeParams): Promise<ResizeResult>;
  sessionAck(ack: SessionAck): void;
  sessionTakeControl(params: TakeControlParams): Promise<TakeControlResult>;
  /**
   * 이 창이 지금 보고 있는 세션을 데몬에 알린다(08-pressure-relief §1).
   * `session_id: null`은 "이 창은 아무 세션도 보고 있지 않다"는 뜻이다.
   * 압력 완화 판단에만 쓰이므로 실패는 호출부가 조용히 흡수한다.
   */
  sessionFocus(params: SessionFocusParams): Promise<SessionFocusResult>;
  /**
   * 한 세션의 압력 완화를 사람이 직접 바꾼다(08-pressure-relief §2).
   * `yield`는 수동 양보(자동 복원 없음), `restore`는 즉시 복원(압력이 남아
   * 있으면 이번 에피소드 동안 보호), `protect`/`unprotect`는 고정 보호
   * 표시다(`protect`는 양보 중인 세션을 함께 복원한다). 데몬은 모르는·끝난
   * 세션을 INVALID_ARGUMENT로 거절한다.
   */
  sessionRelief(params: SessionReliefParams): Promise<SessionReliefResult>;
  /**
   * 자동 양보 정책 토글(08 §2 `relief.set_policy`). 끄면 새 양보만 멈추고,
   * 이미 양보된 세션은 회복 경로나 수동 복원으로 돌아온다.
   */
  reliefSetPolicy(params: ReliefPolicyParams): Promise<ReliefPolicy>;
  /**
   * 자원 가드 수동 일시정지(08 §5). 수동 정지는 자동 재개 대상이 아니다.
   */
  workloadSuspend(params: WorkloadSuspendParams): Promise<WorkloadGuardResult>;
  /** 자원 가드 일시정지 해제(08 §5). */
  workloadResume(params: WorkloadSuspendParams): Promise<WorkloadGuardResult>;
  /** 자원 가드 정책 변경(08 §5 `guard.set_policy`). */
  guardSetPolicy(params: GuardPolicyParams): Promise<GuardPolicy>;
  retentionSetLimit(params: RetentionSetLimitParams): Promise<RetentionSetLimitResult>;
  /** 최근 개입 알림(재시작 후 알림센터 복원용 — W1-5). */
  interventionList(): Promise<InterventionNotice[]>;
  /**
   * 기록된 AI 에이전트 세션 목록(01 §6 `agent_session.list`). 데몬이 이미
   * (agent, agent_session_id)로 중복을 제거해 돌려주고, `active`는 지금
   * 살아 있는 워크로드가 그 세션을 돌리고 있다는 뜻이다. 구 데몬에는
   * 메서드가 없으므로 호출부는 실패를 빈 목록으로 흡수한다.
   */
  agentSessionList(params?: AgentSessionListParams): Promise<AgentSessionRecord[]>;
  /** 목록에서 한 건 제거(`agent_session.forget`) — 기록만 지우고 실행은 건드리지 않는다. */
  agentSessionForget(params: AgentSessionForgetParams): Promise<AgentSessionForgetResult>;
  /** 저널 전체 substring 검색(W2 — 스크롤백 밖 과거 출력). */
  sessionSearch(params: SessionSearchParams): Promise<SessionSearchResult>;

  // ------------------------------------------------------------ O1 mission RPCs
  //
  // O1 orchestration surface (ticket O04, contracts.ts `Rpc`). Mutations are
  // CAS-guarded (`expected_revision`) and idempotent per `request_id`; reads
  // synchronize through immutable snapshots plus the `mission.events` tail.
  // Seq/revision values are decimal wire strings (U64String), never numbers.

  /** Create a draft mission from a staged goal artifact; emits `mission.changed`. */
  repositoryInspect(params: RepositoryInspectParams): Promise<RepositoryInspectResult>;
  missionCreate(params: MissionCreateParams): Promise<MutationResult>;
  missionList(params: MissionListParams): Promise<MissionListResult>;
  /** Materialize (or page through) an immutable entity snapshot, 50 entities per page. */
  missionSnapshot(params: MissionSnapshotParams): Promise<SnapshotPage>;
  /** Committed events strictly after `after_seq`, with the current high watermark. */
  missionEvents(params: MissionEventsParams): Promise<MissionEventsResult>;
  missionControl(params: MissionControlParams): Promise<MutationResult>;
  missionAccept(params: MissionAcceptParams): Promise<MutationResult>;
  missionMessage(params: MissionMessageParams): Promise<MutationResult>;
  missionPlanApply(params: MissionPlanApplyParams): Promise<MutationResult>;
  missionTaskControl(params: MissionTaskControlParams): Promise<MutationResult>;
  missionPolicyUpdate(params: MissionPolicyUpdateParams): Promise<MutationResult>;
  missionFindingResolve(params: MissionFindingResolveParams): Promise<MutationResult>;
  missionDecisionAnswer(params: MissionDecisionAnswerParams): Promise<MutationResult>;
  /** Timeout recovery: replay the stored first response of a mutation (01 §2). */
  missionRequestGet(params: MissionRequestGetParams): Promise<MissionRequestGetResult>;
  /** Tail one run's bounded activity spool (immutable chunk refs, 01 §4). */
  missionActivity(params: MissionActivityParams): Promise<MissionActivityResult>;
  /**
   * The user confirms an unknown/interrupted run's process is gone although the
   * daemon has no termination evidence (`mission.run.attest_exited`). Releases
   * the run's slot/lease/reservation as `user_attested`; the provider outcome
   * and external effects stay unknown. INVALID_STATE +
   * `attestation_not_applicable` when the run is not such a candidate.
   */
  missionRunAttestExited(params: MissionRunAttestExitedParams): Promise<MutationResult>;
  /** Bounded size estimate of daemon-owned mission worktrees still on disk. */
  workspaceUsage(params: WorkspaceUsageParams): Promise<WorkspaceUsageResult>;
  /**
   * User-requested removal of a terminal mission's daemon-owned worktrees.
   * Dirty/unowned/active ones are kept with a reason; accepted result commits
   * stay fetchable. Never touches the user's checkout.
   */
  workspaceCleanup(params: WorkspaceCleanupParams): Promise<WorkspaceCleanupResult>;
  bindingList(): Promise<BindingListResult>;
  /** Create with `expected_revision` "0"; otherwise CAS against the stored revision. */
  bindingSave(params: BindingSaveParams): Promise<BindingSaveResult>;
  /** Cheap probe: install metadata/auth only, never a paid inference call. */
  bindingProbe(params: BindingProbeParams): Promise<BindingProbeResult>;
  /**
   * Read-only discovery of installed codex/claude/opencode CLIs (always in that
   * order): absolute path, local `--version`, login-marker presence, configured
   * default model, and roles an unsaved subscription binding would pass the
   * start gate for. Stores nothing and never returns credential contents.
   */
  runtimeDetect(): Promise<RuntimeDetectResult>;
  templateList(params: TemplateListParams): Promise<TemplateListResult>;
  templateSave(params: TemplateSaveParams): Promise<TemplateSaveResult>;
  verificationList(params: VerificationListParams): Promise<VerificationListResult>;
  verificationSave(params: VerificationSaveParams): Promise<VerificationSaveResult>;
  /** Open a chunked upload; `chunk_bytes`-sized base64 writes follow. */
  artifactBegin(params: ArtifactBeginParams): Promise<ArtifactBeginResult>;
  /** Append at the expected offset only; re-sent identical bytes are idempotent. */
  artifactWrite(params: ArtifactWriteParams): Promise<ArtifactWriteResult>;
  /** Verify length + SHA-256, then seal the upload into an immutable artifact. */
  artifactCommit(params: ArtifactCommitParams): Promise<ArtifactRef>;
  artifactRead(params: ArtifactReadParams): Promise<ArtifactReadResult>;
}

/** Narrow client surface the session pipeline depends on (test seam). */
export interface PipelineClient {
  sessionAttach(params: AttachParams): Promise<AttachResult>;
  sessionInput(params: InputParams): Promise<InputResult>;
  sessionResize(params: ResizeParams): Promise<ResizeResult>;
  sessionAck(ack: SessionAck): void;
}
