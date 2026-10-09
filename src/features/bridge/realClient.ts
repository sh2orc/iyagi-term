import type { RepositoryInspectParams } from "../../generated/RepositoryInspectParams";
import type { RepositoryInspectResult } from "../../generated/RepositoryInspectResult";
/**
 * Real `DaemonClient` over the Tauri bridge (ticket I04).
 *
 * Transport only — ordering, replay gating and write-callback ACKs live in
 * `features/terminal/pipeline.ts` (02-runner §4); channel delivery is NOT
 * an ACK. Mapping:
 * - RPCs          → invoke('bridge_rpc') — id-matched Rust-side, 5s timeout
 * - lifecycle     → invoke('bridge_connect') + invoke('bridge_open_data')
 *                   (data_token TTL 5s → the client opens data immediately)
 * - control events→ invoke('bridge_subscribe_events') + Channel
 * - session output→ invoke('bridge_subscribe_session') + per-VIEW Channel
 *                   (registered BEFORE attach so eager replay records are
 *                   never dropped bridge-side; the pipeline buffers them).
 *                   The bridge keys channels by view: a re-attach replaces
 *                   the previous channel and `bridge_unsubscribe_session`
 *                   drops it, so long uptimes never accumulate channels.
 * - ACKs          → invoke('bridge_ack') on the data connection
 *
 * DAEMON_UNAVAILABLE triggers exactly one transparent retry: disconnect →
 * reconnect → same id (request ids are idempotency keys, 01 §7 — a launch
 * replayed with the same request_id never runs the CLI twice).
 */

import type { ErrorCode } from "../../generated/ErrorCode";
import type { HostSample } from "../../generated/HostSample";
import type { SessionOutput } from "../../generated/SessionOutput";
import type { Snapshot } from "../../generated/Snapshot";
import type { SnapshotPage } from "../../generated/SnapshotPage";
import type { InterventionNotice } from "../../generated/InterventionNotice";
import type { AgentSessionForgetParams } from "../../generated/AgentSessionForgetParams";
import type { AgentSessionForgetResult } from "../../generated/AgentSessionForgetResult";
import type { AgentSessionListParams } from "../../generated/AgentSessionListParams";
import type { AgentSessionRecord } from "../../generated/AgentSessionRecord";
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
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import type { ArtifactBeginParams } from "../../generated/ArtifactBeginParams";
import type { ArtifactBeginResult } from "../../generated/ArtifactBeginResult";
import type { ArtifactCommitParams } from "../../generated/ArtifactCommitParams";
import type { ArtifactReadParams } from "../../generated/ArtifactReadParams";
import type { ArtifactReadResult } from "../../generated/ArtifactReadResult";
import type { ArtifactRef } from "../../generated/ArtifactRef";
import type { ArtifactWriteParams } from "../../generated/ArtifactWriteParams";
import type { ArtifactWriteResult } from "../../generated/ArtifactWriteResult";
import type { BindingListResult } from "../../generated/BindingListResult";
import type { BindingProbeParams } from "../../generated/BindingProbeParams";
import type { BindingProbeResult } from "../../generated/BindingProbeResult";
import type { BindingSaveParams } from "../../generated/BindingSaveParams";
import type { BindingSaveResult } from "../../generated/BindingSaveResult";
import type { MissionErrorCode } from "../../generated/MissionErrorCode";
import type { MissionAcceptParams } from "../../generated/MissionAcceptParams";
import type { MissionActivityParams } from "../../generated/MissionActivityParams";
import type { MissionActivityResult } from "../../generated/MissionActivityResult";
import type { MissionControlParams } from "../../generated/MissionControlParams";
import type { MissionCreateParams } from "../../generated/MissionCreateParams";
import type { MissionDecisionAnswerParams } from "../../generated/MissionDecisionAnswerParams";
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
import type { RuntimeDetectResult } from "../../generated/RuntimeDetectResult";
import type { TemplateListParams } from "../../generated/TemplateListParams";
import type { TemplateListResult } from "../../generated/TemplateListResult";
import type { TemplateSaveParams } from "../../generated/TemplateSaveParams";
import type { TemplateSaveResult } from "../../generated/TemplateSaveResult";
import type { VerificationListParams } from "../../generated/VerificationListParams";
import type { VerificationListResult } from "../../generated/VerificationListResult";
import type { VerificationSaveParams } from "../../generated/VerificationSaveParams";
import type { VerificationSaveResult } from "../../generated/VerificationSaveResult";
import type { WorkspaceCleanupParams } from "../../generated/WorkspaceCleanupParams";
import type { WorkspaceCleanupResult } from "../../generated/WorkspaceCleanupResult";
import type { WorkspaceUsageParams } from "../../generated/WorkspaceUsageParams";
import type { WorkspaceUsageResult } from "../../generated/WorkspaceUsageResult";
import type {
  CancelParams,
  CancelResult,
  DaemonClient,
  DaemonEvent,
  DaemonEventListener,
  EventSubscription,
  LaunchOutcome,
  ProcessesParams,
  ProcessesResult,
  ReprioritizeParams,
  ReprioritizeResult,
  RetentionSetLimitParams,
  RetentionSetLimitResult,
  RpcErrorDetails,
  TakeControlParams,
  TakeControlResult,
  UpdatePolicyParams,
  UpdatePolicyResult,
} from "../daemon/client";
import { RpcClientError } from "../daemon/client";
import type { AttachParams } from "../../generated/AttachParams";
import type { AttachResult } from "../../generated/AttachResult";
import type { InputParams } from "../../generated/InputParams";
import type { InputResult } from "../../generated/InputResult";
import type { LaunchRequest } from "../../generated/LaunchRequest";
import type { ResizeParams } from "../../generated/ResizeParams";
import type { ResizeResult } from "../../generated/ResizeResult";
import type { SessionAck } from "../../generated/SessionAck";
import { MockDaemonClient } from "../daemon/mockClient";
import type { IpcAdapter } from "./ipc";
import { isTauri, tauriIpcAdapter } from "./ipc";

/** Wire envelope of both bridge channel kinds: `{event, payload}`. */
interface BridgeEventEnvelope {
  event: string;
  payload: unknown;
}

/** Serialized `term_contracts::error::RpcError` rejected by invoke. */
interface RpcErrorShape {
  code: string;
  message: string;
  retryable: boolean;
  details?: unknown;
}

type EventPayload<K extends DaemonEvent["kind"]> = Extract<DaemonEvent, { kind: K }>["payload"];

/** Per-call hooks for the single transparent retry. */
interface RpcHooks {
  /** Runs after a DAEMON_UNAVAILABLE reconnect, before the retried call. */
  afterReconnect?: () => Promise<void>;
}

export class RealDaemonClient implements DaemonClient {
  readonly events = {
    subscribe: (listener: DaemonEventListener): EventSubscription => {
      this.listeners.add(listener);
      void this.ensureConnected().catch(() => undefined);
      return {
        dispose: () => {
          this.listeners.delete(listener);
        },
      };
    },
  };

  private readonly listeners = new Set<DaemonEventListener>();
  private connectPromise: Promise<void> | null = null;
  private reconnectPromise: Promise<void> | null = null;
  private eventChannel: unknown = null;
  private transportGeneration = 0;
  private disposed = false;
  private readonly uuid: () => string;

  constructor(
    private readonly ipc: IpcAdapter = tauriIpcAdapter,
    uuid: () => string = () => crypto.randomUUID(),
  ) {
    this.uuid = uuid;
  }

  // ---------------------------------------------------------------- lifecycle

  private async ensureConnected(): Promise<void> {
    if (this.disposed) throw new Error("daemon client disposed");
    if (!this.connectPromise) {
      this.connectPromise = (async () => {
        // bridge_connect returns HelloResult; the data_token TTL is 5s, so
        // the data connection opens immediately after it (01 §3).
        await this.ipc.invoke("bridge_connect");
        await this.ipc.invoke("bridge_open_data");
        if (this.eventChannel === null) {
          const channel = this.ipc.channel<BridgeEventEnvelope>((message) =>
            this.dispatchEnvelope(message),
          );
          await this.ipc.invoke("bridge_subscribe_events", { onEvent: channel });
          this.eventChannel = channel;
        }
      })().catch(async (error: unknown) => {
        this.connectPromise = null;
        this.eventChannel = null;
        // A half-open bridge (control up, data token already minted/burned)
        // would make every later bridge_open_data fail with INVALID_STATE;
        // tear it down so the next attempt re-hellos with a fresh token.
        await this.ipc.invoke("bridge_disconnect").catch(() => undefined);
        throw toClientError(error);
      });
    }
    return this.connectPromise;
  }

  /** Single-flight reconnect: full disconnect → fresh connect/resubscribe. */
  private reconnect(): Promise<void> {
    if (this.disposed) return Promise.reject(new Error("daemon client disposed"));
    if (!this.reconnectPromise) {
      this.reconnectPromise = (async () => {
        this.connectPromise = null;
        this.eventChannel = null;
        await this.ipc.invoke("bridge_disconnect").catch(() => undefined);
        await this.ensureConnected();
        this.transportGeneration += 1;
      })().finally(() => {
        this.reconnectPromise = null;
      });
    }
    return this.reconnectPromise;
  }

  async transportStatus(): Promise<{
    controlAlive: boolean;
    dataAlive: boolean;
    generation: number;
    daemonOutdated: boolean;
  }> {
    const status = await this.ipc.invoke<{
      control_alive: boolean;
      data_alive: boolean;
      daemon_outdated?: boolean;
    }>("bridge_connection_status");
    return {
      controlAlive: status.control_alive,
      dataAlive: status.data_alive,
      generation: this.transportGeneration,
      daemonOutdated: status.daemon_outdated === true,
    };
  }

  async reconnectTransport(): Promise<void> {
    await this.reconnect();
  }

  /**
   * Retire the current (outdated) daemon without killing its workloads, then
   * reconnect so `bridge_connect` spawns a fresh daemon once the old one is
   * gone (02-runner §1: a redundant instance exits via the singleton lock).
   *
   * The shutdown request goes out through `invoke` directly, NOT the retrying
   * `rpc()` helper: the daemon may drop the control connection as it retires,
   * and a DAEMON_UNAVAILABLE retry there would reconnect and re-send shutdown
   * to the freshly spawned daemon. A dropped request is fine — reconnect
   * brings the new daemon up either way.
   *
   * A daemon that is already current is left alone: when an earlier restart
   * timed out but the fresh daemon came up afterwards, a second click would
   * otherwise retire that fresh daemon and every terminal with it. An unknown
   * status keeps the explicit request.
   */
  async restartDaemon(): Promise<void> {
    await this.ensureConnected();
    const outdated = await this.transportStatus().then((status) => status.daemonOutdated, () => true);
    if (!outdated) return;
    try {
      await this.ipc.invoke("bridge_rpc", {
        id: this.uuid(),
        method: "daemon.shutdown",
        params: { stop_workloads: false },
      });
    } catch {
      // Expected when the daemon retires mid-request; reconnect regardless.
    }
    await this.reconnect();
  }

  /** HMR/controller replacement cleanup. The detached daemon keeps PTYs alive. */
  dispose(): void {
    this.disposed = true;
    this.listeners.clear();
    this.connectPromise = null;
    this.eventChannel = null;
  }

  // ------------------------------------------------------------------- events

  private dispatchEnvelope(message: BridgeEventEnvelope): void {
    // The native bridge groups queued records into one WebView delivery.
    // Keep each journal record intact: the pipeline still owns resize order
    // and acknowledges bytes only after xterm has consumed them.
    if (message.event === "session.output.batch" && Array.isArray(message.payload)) {
      for (const payload of message.payload) {
        // 한 레코드가 dispatch를 깨뜨려도(잘못된 base64 등) 묶음의 나머지까지
        // 버리지 않는다 — 저널 순서에 구멍이 나면 재생이 영영 끝나지 않는다.
        try {
          this.dispatchEnvelope({ event: "session.output", payload });
        } catch (error) {
          console.error("session.output dispatch failed", error);
        }
      }
      return;
    }
    const event = toDaemonEvent(message);
    if (!event) return;
    for (const listener of this.listeners) {
      // 리스너 하나의 예외가 여기서 새면 Tauri Channel은 다음 메시지 번호로
      // 넘어가지 않는다(onmessage가 정상 반환한 뒤에만 증가) — 그 뒤의 모든
      // 이벤트가 pendingMessages에 영영 쌓여 session.exited·출력·스냅샷이 앱
      // 전체에서 끊긴다. 한 리스너의 실패는 그 이벤트 하나로 끝낸다.
      try {
        listener(event);
      } catch (error) {
        console.error("daemon event listener failed", { event: message.event, error });
      }
    }
  }

  // --------------------------------------------------------------------- rpc

  private async rpc<T>(
    method: string,
    params: unknown,
    hooks: RpcHooks = {},
    retryAllowed = true,
  ): Promise<T> {
    await this.ensureConnected();
    const id = this.uuid();
    try {
      return await this.ipc.invoke<T>("bridge_rpc", { id, method, params });
    } catch (error) {
      const rpcError = asRpcError(error);
      if (rpcError && retryAllowed && rpcError.retryable) {
        if (rpcError.code === "DAEMON_UNAVAILABLE") {
          await this.reconnect().catch(() => undefined);
          // A reconnect clears the bridge's session-channel table; calls
          // that depend on a channel re-register it before retrying, or
          // the retried attach would succeed daemon-side with no route for
          // its output (a silently black terminal).
          await hooks.afterReconnect?.().catch(() => undefined);
        }
        // The retried call reuses its own fresh id; params carry the real
        // idempotency keys (request_id / input_id), 01 §7.
        return this.rpc<T>(method, params, hooks, false);
      }
      throw toClientError(error);
    }
  }

  // --------------------------------------------------------- DaemonClient API

  async systemSnapshot(): Promise<Snapshot> {
    return this.rpc<Snapshot>("system.snapshot", {});
  }

  async workloadLaunch(request: LaunchRequest): Promise<LaunchOutcome> {
    return this.rpc<LaunchOutcome>("workload.launch", request);
  }

  async workloadCancel(params: CancelParams): Promise<CancelResult> {
    return this.rpc<CancelResult>("workload.cancel", params);
  }

  async workloadReprioritize(params: ReprioritizeParams): Promise<ReprioritizeResult> {
    return this.rpc<ReprioritizeResult>("workload.reprioritize", params);
  }

  async workloadUpdatePolicy(params: UpdatePolicyParams): Promise<UpdatePolicyResult> {
    return this.rpc<UpdatePolicyResult>("workload.update_policy", params);
  }

  async workloadProcesses(params: ProcessesParams): Promise<ProcessesResult> {
    return this.rpc<ProcessesResult>("workload.processes", params);
  }

  async sessionAttach(params: AttachParams): Promise<AttachResult> {
    await this.ensureConnected();
    // Register the per-view output channel BEFORE attach: the daemon
    // starts pushing replay records the moment attach lands (01 §3), and the
    // pipeline explicitly buffers records that arrive before the reply.
    await this.subscribeSession(params.session_id, params.view_id);
    return this.rpc<AttachResult>("session.attach", params, {
      afterReconnect: () => this.subscribeSession(params.session_id, params.view_id),
    });
  }

  private async subscribeSession(sessionId: string, viewId: string): Promise<void> {
    const channel = this.ipc.channel<BridgeEventEnvelope>((message) =>
      this.dispatchEnvelope(message),
    );
    await this.ipc.invoke("bridge_subscribe_session", {
      sessionId,
      viewId,
      onOutput: channel,
    });
  }

  async sessionDetach(params: { session_id: string; view_id: string }): Promise<{ detached: true }> {
    return this.rpc<{ detached: true }>("session.detach", params);
  }

  /** Drop this view's bridge channel (fire-and-forget; unknown ids are no-ops). */
  sessionUnsubscribe(params: { session_id: string; view_id: string }): void {
    void this.ipc
      .invoke("bridge_unsubscribe_session", {
        sessionId: params.session_id,
        viewId: params.view_id,
      })
      .catch(() => undefined);
  }

  async sessionInput(params: InputParams): Promise<InputResult> {
    return this.rpc<InputResult>("session.input", params);
  }

  async sessionResize(params: ResizeParams): Promise<ResizeResult> {
    return this.rpc<ResizeResult>("session.resize", params);
  }

  /** Fire-and-forget on the data connection (정상 처리 시 별도 응답 없음). */
  sessionAck(ack: SessionAck): void {
    void this.ipc
      .invoke("bridge_ack", {
        sessionId: ack.session_id,
        epoch: ack.epoch,
        throughSeq: ack.through_seq,
      })
      .catch(() => undefined);
  }

  async sessionTakeControl(params: TakeControlParams): Promise<TakeControlResult> {
    return this.rpc<TakeControlResult>("session.take_control", params);
  }

  async sessionFocus(params: SessionFocusParams): Promise<SessionFocusResult> {
    return this.rpc<SessionFocusResult>("session.focus", params);
  }

  /** 수동 완화(08 §2) — 수동이 자동보다 우선한다는 의미는 데몬이 지킨다. */
  async sessionRelief(params: SessionReliefParams): Promise<SessionReliefResult> {
    return this.rpc<SessionReliefResult>("session.relief", params);
  }

  /** 자동 양보 정책 토글(08 §2). 결과가 데몬이 적용한 값이다. */
  async reliefSetPolicy(params: ReliefPolicyParams): Promise<ReliefPolicy> {
    return this.rpc<ReliefPolicy>("relief.set_policy", params);
  }

  /** 자원 가드 수동 일시정지/해제(08 §5) — 수동 정지는 자동 재개 대상이 아니다. */
  async workloadSuspend(params: WorkloadSuspendParams): Promise<WorkloadGuardResult> {
    return this.rpc<WorkloadGuardResult>("workload.suspend", params);
  }

  async workloadResume(params: WorkloadSuspendParams): Promise<WorkloadGuardResult> {
    return this.rpc<WorkloadGuardResult>("workload.resume", params);
  }

  /** 자원 가드 정책 변경(08 §5). */
  async guardSetPolicy(params: GuardPolicyParams): Promise<GuardPolicy> {
    return this.rpc<GuardPolicy>("guard.set_policy", params);
  }

  async retentionSetLimit(params: RetentionSetLimitParams): Promise<RetentionSetLimitResult> {
    return this.rpc<RetentionSetLimitResult>("retention.set_limit", params);
  }

  async interventionList(): Promise<InterventionNotice[]> {
    return this.rpc<InterventionNotice[]>("intervention.list", {});
  }

  async sessionSearch(params: SessionSearchParams): Promise<SessionSearchResult> {
    return this.rpc<SessionSearchResult>("session.search", params as unknown as Record<string, unknown>);
  }

  /** 기록된 에이전트 세션 목록(01 §6). 구 데몬은 METHOD_NOT_FOUND로 거절한다. */
  async agentSessionList(params: AgentSessionListParams = {}): Promise<AgentSessionRecord[]> {
    return this.rpc<AgentSessionRecord[]>("agent_session.list", params);
  }

  async agentSessionForget(params: AgentSessionForgetParams): Promise<AgentSessionForgetResult> {
    return this.rpc<AgentSessionForgetResult>("agent_session.forget", params);
  }

  // ------------------------------------------------- O1 mission RPCs (O04)
  //
  // Transport only: CAS retries, snapshot paging policy and the
  // mission.events tail live in the mission store. U64 wire values travel
  // as decimal strings end to end — no Number coercion here.

  async repositoryInspect(params: RepositoryInspectParams): Promise<RepositoryInspectResult> {
    return this.rpc<RepositoryInspectResult>("repository.inspect", params);
  }

  async missionCreate(params: MissionCreateParams): Promise<MutationResult> {
    return this.rpc<MutationResult>("mission.create", params);
  }

  async missionList(params: MissionListParams): Promise<MissionListResult> {
    return this.rpc<MissionListResult>("mission.list", params);
  }

  async missionSnapshot(params: MissionSnapshotParams): Promise<SnapshotPage> {
    return this.rpc<SnapshotPage>("mission.snapshot", params);
  }

  async missionEvents(params: MissionEventsParams): Promise<MissionEventsResult> {
    return this.rpc<MissionEventsResult>("mission.events", params);
  }

  async missionControl(params: MissionControlParams): Promise<MutationResult> {
    return this.rpc<MutationResult>("mission.control", params);
  }

  async missionAccept(params: MissionAcceptParams): Promise<MutationResult> {
    return this.rpc<MutationResult>("mission.accept", params);
  }

  async missionMessage(params: MissionMessageParams): Promise<MutationResult> {
    return this.rpc<MutationResult>("mission.message", params);
  }

  async missionPlanApply(params: MissionPlanApplyParams): Promise<MutationResult> {
    return this.rpc<MutationResult>("mission.plan.apply", params);
  }

  async missionTaskControl(params: MissionTaskControlParams): Promise<MutationResult> {
    return this.rpc<MutationResult>("mission.task.control", params);
  }

  async missionPolicyUpdate(params: MissionPolicyUpdateParams): Promise<MutationResult> {
    return this.rpc<MutationResult>("mission.policy.update", params);
  }

  async missionFindingResolve(params: MissionFindingResolveParams): Promise<MutationResult> {
    return this.rpc<MutationResult>("mission.finding.resolve", params);
  }

  async missionDecisionAnswer(params: MissionDecisionAnswerParams): Promise<MutationResult> {
    return this.rpc<MutationResult>("mission.decision.answer", params);
  }

  async missionRequestGet(params: MissionRequestGetParams): Promise<MissionRequestGetResult> {
    return this.rpc<MissionRequestGetResult>("mission.request.get", params);
  }

  async missionActivity(params: MissionActivityParams): Promise<MissionActivityResult> {
    return this.rpc<MissionActivityResult>("mission.activity", params);
  }

  async missionRunAttestExited(params: MissionRunAttestExitedParams): Promise<MutationResult> {
    return this.rpc<MutationResult>("mission.run.attest_exited", params);
  }

  async workspaceUsage(params: WorkspaceUsageParams): Promise<WorkspaceUsageResult> {
    return this.rpc<WorkspaceUsageResult>("workspace.usage", params);
  }

  async workspaceCleanup(params: WorkspaceCleanupParams): Promise<WorkspaceCleanupResult> {
    return this.rpc<WorkspaceCleanupResult>("workspace.cleanup", params);
  }

  async bindingList(): Promise<BindingListResult> {
    return this.rpc<BindingListResult>("binding.list", {});
  }

  async bindingSave(params: BindingSaveParams): Promise<BindingSaveResult> {
    return this.rpc<BindingSaveResult>("binding.save", params);
  }

  async bindingProbe(params: BindingProbeParams): Promise<BindingProbeResult> {
    return this.rpc<BindingProbeResult>("binding.probe", params);
  }

  async runtimeDetect(): Promise<RuntimeDetectResult> {
    return this.rpc<RuntimeDetectResult>("runtime.detect", {});
  }

  async templateList(params: TemplateListParams): Promise<TemplateListResult> {
    return this.rpc<TemplateListResult>("template.list", params);
  }

  async templateSave(params: TemplateSaveParams): Promise<TemplateSaveResult> {
    return this.rpc<TemplateSaveResult>("template.save", params);
  }

  async verificationList(params: VerificationListParams): Promise<VerificationListResult> {
    return this.rpc<VerificationListResult>("verification.list", params);
  }

  async verificationSave(params: VerificationSaveParams): Promise<VerificationSaveResult> {
    return this.rpc<VerificationSaveResult>("verification.save", params);
  }

  async artifactBegin(params: ArtifactBeginParams): Promise<ArtifactBeginResult> {
    return this.rpc<ArtifactBeginResult>("artifact.begin", params);
  }

  async artifactWrite(params: ArtifactWriteParams): Promise<ArtifactWriteResult> {
    return this.rpc<ArtifactWriteResult>("artifact.write", params);
  }

  async artifactCommit(params: ArtifactCommitParams): Promise<ArtifactRef> {
    return this.rpc<ArtifactRef>("artifact.commit", params);
  }

  async artifactRead(params: ArtifactReadParams): Promise<ArtifactReadResult> {
    return this.rpc<ArtifactReadResult>("artifact.read", params);
  }
}

/** Map a bridge channel envelope to the typed event union; null if unknown. */
function toDaemonEvent(envelope: BridgeEventEnvelope): DaemonEvent | null {
  if (!envelope || typeof envelope.event !== "string") return null;
  const payload = envelope.payload;
  switch (envelope.event) {
    case "workload.changed":
      return { kind: "workload.changed", payload: payload as WorkloadSummary };
    case "queue.changed":
      return { kind: "queue.changed", payload: payload as EventPayload<"queue.changed"> };
    case "resource.snapshot": {
      // The daemon wraps periodic samples in {revision, host, pressure}.
      // UI consumers take HostSample directly, with the classified pressure.
      const snapshot = payload as { host: HostSample; pressure: HostSample["pressure"] };
      return { kind: "resource.snapshot", payload: { ...snapshot.host, pressure: snapshot.pressure } };
    }
    case "session.output":
      return { kind: "session.output", payload: payload as SessionOutput };
    case "session.resize_applied":
      return { kind: "session.resize_applied", payload: payload as EventPayload<"session.resize_applied"> };
    case "session.flow_blocked":
      return { kind: "session.flow_blocked", payload: payload as EventPayload<"session.flow_blocked"> };
    case "session.exited":
      return { kind: "session.exited", payload: payload as EventPayload<"session.exited"> };
    case "session.owner_changed":
      return { kind: "session.owner_changed", payload: payload as EventPayload<"session.owner_changed"> };
    case "session.replay_required":
      return { kind: "session.replay_required", payload: payload as EventPayload<"session.replay_required"> };
    case "intervention.reported":
      return { kind: "intervention.reported", payload: payload as InterventionNotice };
    case "mission.changed":
      return {
        kind: "mission.changed",
        payload: payload as EventPayload<"mission.changed">,
      };
    default:
      return null;
  }
}

function asRpcError(value: unknown): RpcErrorShape | null {
  if (typeof value !== "object" || value === null) return null;
  const candidate = value as Record<string, unknown>;
  if (
    typeof candidate.code === "string" &&
    typeof candidate.message === "string" &&
    typeof candidate.retryable === "boolean"
  ) {
    return candidate as unknown as RpcErrorShape;
  }
  return null;
}

function toClientError(error: unknown): Error {
  const rpcError = asRpcError(error);
  if (rpcError) {
    return new RpcClientError(
      rpcError.code as ErrorCode | MissionErrorCode,
      rpcError.message,
      rpcError.retryable,
      toErrorDetails(rpcError.details),
    );
  }
  if (error instanceof Error) return error;
  return new Error(String(error));
}

/**
 * Normalize the wire `details` object (01-contracts O1 §7): keep only the
 * known typed fields, drop `null`s (absent means unknown, never zero), and
 * never let odd shapes through.
 */
function toErrorDetails(value: unknown): RpcErrorDetails | undefined {
  if (typeof value !== "object" || value === null) return undefined;
  const raw = value as Record<string, unknown>;
  const details: RpcErrorDetails = {};
  let known = false;
  if (typeof raw.current_revision === "string") {
    details.current_revision = raw.current_revision;
    known = true;
  }
  if (typeof raw.reason_code === "string") {
    details.reason_code = raw.reason_code;
    known = true;
  }
  if (typeof raw.retry_after_ms === "number") {
    details.retry_after_ms = raw.retry_after_ms;
    known = true;
  }
  if (typeof raw.decision_id === "string") {
    details.decision_id = raw.decision_id;
    known = true;
  }
  return known ? details : undefined;
}

/**
 * Pick the transport: real client inside the Tauri webview, mock in a plain
 * browser (dev preview) — existing tests never touch the real path.
 */
export function createDaemonClient(
  adapter: IpcAdapter = tauriIpcAdapter,
  detect: () => boolean = isTauri,
): DaemonClient {
  return detect() ? new RealDaemonClient(adapter) : new MockDaemonClient();
}
