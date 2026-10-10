import { isDeterministicIntegration, taskRunsInPhase } from "../missions/executionKind";
import type { RepositoryInspectParams } from "../../generated/RepositoryInspectParams";
import type { RepositoryInspectResult } from "../../generated/RepositoryInspectResult";
/**
 * In-memory DaemonClient simulation for unit tests and the dev preview.
 *
 * Behavior mirrors the spec sections the UI depends on:
 * - shell sessions echo input back as ordered `session.output` records;
 * - managed launches queue first (no session/PTY until admission — U14);
 * - output delivery is credit-controlled per session: transmission pauses at
 *   256 KiB unacked and resumes at <= 64 KiB (`session.flow_blocked`, 02 §4);
 * - ACKs advance `through_seq`; duplicate ACKs are ignored, future-seq ACKs
 *   are protocol errors (02 §4);
 * - attach returns a fresh epoch and replays the journal from seq 1.
 *
 * Events are flushed through `opts.schedule` (default setTimeout 0) so a
 * caller's `await sessionAttach(...)` continuation always runs before the
 * replayed records arrive — matching the real transport's async delivery.
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
import type { BaseSnapshot } from "../../generated/BaseSnapshot";
import type { Binding } from "../../generated/Binding";
import type { BindingListResult } from "../../generated/BindingListResult";
import type { BindingProbeParams } from "../../generated/BindingProbeParams";
import type { BindingProbeResult } from "../../generated/BindingProbeResult";
import type { BindingSaveParams } from "../../generated/BindingSaveParams";
import type { BindingSaveResult } from "../../generated/BindingSaveResult";
import type { Capabilities } from "../../generated/Capabilities";
import type { Change } from "../../generated/Change";
import type { Decision } from "../../generated/Decision";
import type { Entity } from "../../generated/Entity";
import type { HostSample } from "../../generated/HostSample";
import type { InterventionNotice } from "../../generated/InterventionNotice";
import type { LocalEvidence } from "../../generated/LocalEvidence";
import type { LocalProbeReport } from "../../generated/LocalProbeReport";
import type { Message } from "../../generated/Message";
import type { Mission } from "../../generated/Mission";
import type { MissionAcceptParams } from "../../generated/MissionAcceptParams";
import type { MissionActivityParams } from "../../generated/MissionActivityParams";
import type { MissionActivityResult } from "../../generated/MissionActivityResult";
import type { MissionControlParams } from "../../generated/MissionControlParams";
import type { MissionCreateParams } from "../../generated/MissionCreateParams";
import type { MissionDecisionAnswerParams } from "../../generated/MissionDecisionAnswerParams";
import type { MissionEvent } from "../../generated/MissionEvent";
import type { MissionEventType } from "../../generated/MissionEventType";
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
import type { ProbeModel } from "../../generated/ProbeModel";
import type { RuntimeDetectResult } from "../../generated/RuntimeDetectResult";
import type { SessionFocusParams } from "../../generated/SessionFocusParams";
import type { SessionFocusResult } from "../../generated/SessionFocusResult";
import type { SessionReliefParams } from "../../generated/SessionReliefParams";
import type { SessionReliefResult } from "../../generated/SessionReliefResult";
import type { ReliefPolicy } from "../../generated/ReliefPolicy";
import type { GuardState } from "../../generated/GuardState";
import type { GuardPolicy } from "../../generated/GuardPolicy";
import type { WorkloadSuspendParams } from "../../generated/WorkloadSuspendParams";
import type { WorkloadGuardResult } from "../../generated/WorkloadGuardResult";
import type { ReliefPolicyParams } from "../../generated/ReliefPolicyParams";
import type { ReliefState } from "../../generated/ReliefState";
import type { PressureLevel } from "../../generated/PressureLevel";
import type { SessionSearchParams } from "../../generated/SessionSearchParams";
import type { SessionSearchResult } from "../../generated/SessionSearchResult";
import type { SessionSearchMatch } from "../../generated/SessionSearchMatch";
import type { Snapshot } from "../../generated/Snapshot";
import type { SnapshotPage } from "../../generated/SnapshotPage";
import type { TeamTemplate } from "../../generated/TeamTemplate";
import type { TemplateListParams } from "../../generated/TemplateListParams";
import type { TemplateListResult } from "../../generated/TemplateListResult";
import type { TemplateSaveParams } from "../../generated/TemplateSaveParams";
import type { TemplateSaveResult } from "../../generated/TemplateSaveResult";
import type { VerificationCommand } from "../../generated/VerificationCommand";
import type { VerificationListParams } from "../../generated/VerificationListParams";
import type { VerificationListResult } from "../../generated/VerificationListResult";
import type { VerificationSaveParams } from "../../generated/VerificationSaveParams";
import type { VerificationSaveResult } from "../../generated/VerificationSaveResult";
import type { InputParams } from "../../generated/InputParams";
import type { InputResult } from "../../generated/InputResult";
import type { LaunchRequest } from "../../generated/LaunchRequest";
import type { QueueEntry } from "../../generated/QueueEntry";
import type { QueueReason } from "../../generated/QueueReason";
import type { ResizeParams } from "../../generated/ResizeParams";
import type { ResizeResult } from "../../generated/ResizeResult";
import type { SessionAck } from "../../generated/SessionAck";
import { mockPromptFor } from "../terminal/shellProfiles";
import type { SessionOutput } from "../../generated/SessionOutput";
import type { WorkloadState } from "../../generated/WorkloadState";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import type { WorkspaceCleanupParams } from "../../generated/WorkspaceCleanupParams";
import type { WorkspaceCleanupResult } from "../../generated/WorkspaceCleanupResult";
import type { WorkspaceKept } from "../../generated/WorkspaceKept";
import type { WorkspaceUsageEntry } from "../../generated/WorkspaceUsageEntry";
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
  TakeControlParams,
  TakeControlResult,
  UpdatePolicyParams,
  UpdatePolicyResult,
} from "./client";
import { RpcClientError } from "./client";
import { base64ToBytes, bytesToBase64 } from "./base64";
import { sha256HexAuto } from "./sha256";

/** defaults.json limits */
const OUTPUT_CHUNK_BYTES = 16384;
const OUTPUT_HIGH_BYTES = 262144;
const OUTPUT_LOW_BYTES = 65536;
const MANAGED_CONCURRENCY = 2;
const VIEWS_PER_SESSION = 2;
const MAX_COLS_ROWS = 1000;
/** O1 §4: 스냅샷 page는 최대 50개 entity. */
const SNAPSHOT_PAGE_ENTITIES = 50;
/**
 * runtime.detect fixture의 Codex 증거(버전·모델). bindingProbe가 같은 조합을
 * 데몬 capability registry처럼 검증된 것으로 채우는 데 함께 쓴다.
 */
const MOCK_CODEX_VERSION = "0.154.0";
const MOCK_CODEX_PROVEN_MODEL = "gpt-5.6-luna";
/** runtime.detect/bindingProbe fixture가 각 런타임이 광고한다고 흉내내는 모델 id 목록. */
const MOCK_CODEX_MODELS = ["gpt-6-astra", "gpt-5.6-sol", MOCK_CODEX_PROVEN_MODEL];
/** runtime.detect fixture의 미검증 Claude 버전(실험적 연결 동의 대상). */
const MOCK_CLAUDE_VERSION = "2.1.271";
const MOCK_CLAUDE_MODEL = "claude-opus-5";
const MOCK_CLAUDE_MODELS = ["opus", "sonnet", "haiku"];
const MOCK_OPENCODE_MODELS = ["glm-5.3"];
/** runtime.detect fixture의 Claude Z.ai 경로(ccg 라우팅) 후보. */
const MOCK_CLAUDE_ZAI_MODELS = ["glm-5.3"];
/** LocalEvidence.os — 데몬이 관측값으로 채우는 자리(11 §3.3). */
const MOCK_OS = "macos";
/** Codex 자가 진단의 sandbox 사례 수(POSIX, python3 있음 — 11 §6). */
const MOCK_CODEX_SANDBOX_CASES = 12;
/** workspace.usage mock: 디스크에 남은 작업 공간 하나를 1 MiB로 센다. */
const MOCK_WORKSPACE_BYTES = 1_048_576;
/** 데몬 capability_evidence::adapter_capabilities(03 §6 표)의 사본. */
const MOCK_ADAPTER_IMPLEMENTED: Record<"codex" | "claude" | "opencode", ReadonlyArray<keyof Binding["capabilities"]>> = {
  codex: ["structured_result", "events", "cancel", "steer", "approval_reply", "read_only", "scoped_write", "model_listing", "usage"],
  claude: ["structured_result", "events", "cancel", "read_only", "scoped_write", "usage"],
  opencode: ["structured_result", "events", "cancel", "approval_reply", "read_only", "scoped_write", "usage"],
};
/** O1 §4: 미션당 캐시 스냅샷 2개, 60초 만료. */
const SNAPSHOTS_PER_MISSION = 2;
const SNAPSHOT_TTL_MS = 60_000;
/** O1 §3: artifact.write는 4 KiB raw chunk를 base64로 전송. */
const ARTIFACT_CHUNK_BYTES = 4096;

export interface MockDaemonOptions {
  managedConcurrency?: number;
  outputHighBytes?: number;
  outputLowBytes?: number;
  /** admit queued managed workloads automatically (default true). */
  autoAdmitQueue?: boolean;
  /** host resource.snapshot cadence; 0 disables. Default 1000. */
  resourceIntervalMs?: number;
  /** scheduler for event flush / async transitions. Default setTimeout 0. */
  schedule?: (fn: () => void) => void;
  uuid?: () => string;
  /** monotonic clock in ms. Default performance.now-ish via Date. */
  monotonicNow?: () => number;
}

interface MockRecord {
  seq: number;
  kind: "output" | "resize";
  bytes: Uint8Array | null;
  cols?: number;
  rows?: number;
}

interface MockView {
  viewId: string;
  access: "writer" | "reader";
}

interface MockSession {
  id: string;
  workloadId: string;
  cwd: string;
  epoch: string;
  records: MockRecord[];
  views: Map<string, MockView>;
  writerViewId: string | null;
  cols: number;
  rows: number;
  exited: boolean;
  /** per-epoch consumer ledger (mock models a single data connection). */
  ackedSeq: number;
  lastDeliveredSeq: number;
  unackedBytes: number;
  blocked: boolean;
  retentionMaxBytes: string;
}

interface MockWorkload {
  id: string;
  requestId: string;
  request: LaunchRequest;
  mode: "shell" | "managed";
  state: WorkloadState;
  queueReason: QueueReason | null;
  cancelRequested: boolean;
  rootExited: boolean;
  exitCode: number | null;
  lastErrorCode: string | null;
  sessionId: string | null;
  /** 압력 완화 상태(08 §2) — mock에서는 수동 액션만 이 값을 바꾼다. */
  relief: ReliefState;
  guard: GuardState;
  /** 자동 완화 제외 표시(08 §0-3). */
  protected: boolean;
}

/** O1 mission fake store(O04) — 현재 projection entity + 커밋된 이벤트. */
interface MockMissionState {
  mission: Mission;
  /** Insertion-ordered entity map keyed by entity id. */
  entities: Map<string, Entity>;
  events: MissionEvent[];
}

/** Materialized read snapshot(O1 §4) — 만들어진 순간의 불변 entity 목록. */
interface MockSnapshot {
  snapshotId: string;
  missionId: string;
  entities: Entity[];
  atSeq: string;
  revision: string;
  expiresAtMs: number;
}

/** 진행 중 artifact upload(O1 §3). */
interface MockUpload {
  uploadId: string;
  missionId: string | null;
  mediaType: string;
  expectedBytes: number;
  expectedSha256: string;
  bytes: Uint8Array;
  /** commit 재호출은 같은 ref를 돌려준다(O1 §3). */
  committed: ArtifactRef | null;
}

/** request_id 멱등 원장(O1 §2) — 저장된 최초 응답과 지문. */
interface MockRequestEntry {
  fingerprint: string;
  result: unknown;
}

/** Mission mutation params 공통부(O1 contracts.ts `Mutation`). */
interface MockMutationParams {
  request_id: string;
  mission_id: string;
  expected_revision: string;
}

export class MockDaemonClient implements DaemonClient {
  readonly events = {
    subscribe: (listener: DaemonEventListener): EventSubscription => {
      this.listeners.add(listener);
      this.startResourceStream();
      return {
        dispose: () => {
          this.listeners.delete(listener);
          if (this.listeners.size === 0) this.stopResourceStream();
        },
      };
    },
  };

  /** 02 §4 protocol violations surfaced for tests/diagnostics. */
  readonly protocolErrors: Array<{ session_id: string; detail: string }> = [];

  private readonly opts: Required<
    Pick<MockDaemonOptions, "managedConcurrency" | "outputHighBytes" | "outputLowBytes" | "autoAdmitQueue" | "resourceIntervalMs">
  > & { schedule: (fn: () => void) => void; uuid: () => string; monotonicNow: () => number };

  private readonly sessions = new Map<string, MockSession>();
  private readonly workloads = new Map<string, MockWorkload>();
  private readonly byRequestId = new Map<string, string>();
  private readonly listeners = new Set<DaemonEventListener>();
  private readonly queueOrder: string[] = [];
  // ---------------------------------------------------------- O1 mission store
  private readonly missionStates = new Map<string, MockMissionState>();
  private readonly snapshots = new Map<string, MockSnapshot>();
  private readonly uploads = new Map<string, MockUpload>();
  private readonly artifacts = new Map<string, { ref: ArtifactRef; bytes: Uint8Array }>();
  private readonly requests = new Map<string, MockRequestEntry>();
  private readonly bindings = new Map<string, Binding>();
  private readonly templates = new Map<string, TeamTemplate>();
  private readonly verificationCommands = new Map<string, VerificationCommand>();
  /** repository registry — 첫 mission.create가 배정한다(O1 §3). */
  private readonly repositoryIds = new Map<string, string>();
  /** repositoryInspect가 마지막으로 본 더러운 경로 개수(경로별) — include_uncommitted의 entry_count 흉내. */
  private readonly dirtyEntryCounts = new Map<string, number>();
  /** workspace.cleanup가 지운 작업 공간 ID(디스크 상태 흉내). */
  private readonly removedWorkspaces = new Set<string>();
  private pendingEvents: DaemonEvent[] = [];
  private flushScheduled = false;
  /** 이 mock 인스턴스(=제어 연결 하나)가 보고 있는 세션(08 §1). */
  private focusedSessionId: string | null = null;
  /** 데몬 전체의 완화 정책(08 §2) — 기본은 defaults.json과 같은 auto_yield. */
  private reliefPolicy: ReliefPolicy = { auto_yield: true };
  private guardPolicy: GuardPolicy = {
    auto_suspend: true,
    cpu_cores_limit: 6,
    rss_limit_bytes: "4294967296",
    sustain_ms: "20000",
    rss_sustain_ms: "5000",
    auto_resume: false,
  };
  private revision = 1;
  private resourceTimer: ReturnType<typeof setInterval> | null = null;
  private resourceTick = 0;
  private hostSample: HostSample | null = null;
  private disposed = false;

  constructor(options: MockDaemonOptions = {}) {
    this.opts = {
      managedConcurrency: options.managedConcurrency ?? MANAGED_CONCURRENCY,
      outputHighBytes: options.outputHighBytes ?? OUTPUT_HIGH_BYTES,
      outputLowBytes: options.outputLowBytes ?? OUTPUT_LOW_BYTES,
      autoAdmitQueue: options.autoAdmitQueue ?? true,
      resourceIntervalMs: options.resourceIntervalMs ?? 1000,
      schedule: options.schedule ?? ((fn) => setTimeout(fn, 0)),
      uuid: options.uuid ?? (() => crypto.randomUUID()),
      monotonicNow: options.monotonicNow ?? (() => Date.now()),
    };
  }

  dispose(): void {
    this.disposed = true;
    this.stopResourceStream();
  }

  // ---------------------------------------------------------------- snapshot

  async systemSnapshot(): Promise<Snapshot> {
    if (!this.hostSample) this.hostSample = this.nextHostSample();
    return {
      revision: this.revision,
      host: this.hostSample,
      workloads: [...this.workloads.values()].map((w) => this.workloadSummary(w)),
      queue: this.queueEntries(),
      capabilities: mockCapabilities(),
      reconciliation_required: false,
      focused_session_ids: this.focusedSessionIds(),
      relief_policy: { ...this.reliefPolicy },
      guard_policy: { ...this.guardPolicy },
    };
  }

  // ------------------------------------------------------------------ launch

  async workloadLaunch(request: LaunchRequest): Promise<LaunchOutcome> {
    const existing = this.byRequestId.get(request.request_id);
    if (existing) {
      const w = this.workloads.get(existing) as MockWorkload;
      return this.launchOutcome(w);
    }
    const workloadId = this.opts.uuid();
    const w: MockWorkload = {
      id: workloadId,
      requestId: request.request_id,
      request,
      mode: request.mode,
      state: request.mode === "managed" ? "QUEUED" : "RUNNING",
      queueReason: request.mode === "managed" ? "WAIT_TELEMETRY" : null,
      cancelRequested: false,
      rootExited: false,
      exitCode: null,
      lastErrorCode: null,
      sessionId: null,
      relief: { kind: "NONE" },
      guard: { kind: "NONE" },
      protected: false,
    };
    this.workloads.set(workloadId, w);
    this.byRequestId.set(request.request_id, workloadId);
    this.revision++;

    if (request.mode === "managed") {
      this.queueOrder.push(workloadId);
      this.emit({ kind: "queue.changed", payload: { queue: this.queueEntries(), revision: this.revision } });
      this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
      if (this.opts.autoAdmitQueue) this.schedule(() => this.admitTick());
    } else {
      this.createSessionFor(w);
      this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
    }
    this.scheduleFlush();
    return this.launchOutcome(w);
  }

  private launchOutcome(w: MockWorkload): LaunchOutcome {
    return {
      workload_id: w.id,
      session_id: w.sessionId,
      state: w.state,
      effective_policy: w.request.policy,
      missing_capabilities: [],
    };
  }

  /** Admission: sessions/PTY are created only here (U14). */
  admitTick(): void {
    let running = this.runningManagedCount();
    let changed = false;
    while (running < this.opts.managedConcurrency && this.queueOrder.length > 0) {
      const workloadId = this.queueOrder.shift() as string;
      const w = this.workloads.get(workloadId);
      if (!w || w.state !== "QUEUED") continue;
      w.state = "STARTING";
      this.revision++;
      this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
      this.createSessionFor(w);
      w.state = "RUNNING";
      w.queueReason = null;
      running++;
      changed = true;
      this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
    }
    if (changed) {
      this.emit({ kind: "queue.changed", payload: { queue: this.queueEntries(), revision: this.revision } });
      this.scheduleFlush();
    }
  }

  private createSessionFor(w: MockWorkload): void {
    const id = this.opts.uuid();
    const session: MockSession = {
      id,
      workloadId: w.id,
      cwd: w.request.cwd,
      epoch: this.opts.uuid(),
      records: [],
      views: new Map(),
      writerViewId: null,
      cols: w.request.cols,
      rows: w.request.rows,
      exited: false,
      ackedSeq: 0,
      lastDeliveredSeq: 0,
      unackedBytes: 0,
      blocked: false,
      retentionMaxBytes: "134217728",
    };
    this.sessions.set(id, session);
    w.sessionId = id;
    // 첫 record는 initial size(02 §5), then the shell prompt.
    this.appendRecord(session, { kind: "resize", bytes: null, cols: session.cols, rows: session.rows });
    // 배너 문구는 없다 — 실제 셸처럼 프롬프트만 보인다(PowerShell→PS>, cmd→경로>, WSL/unix→$).
    const prompt = new TextEncoder().encode(mockPromptFor(w.request.program, w.request.cwd));
    this.appendRecord(session, { kind: "output", bytes: prompt });
  }

  private runningManagedCount(): number {
    let n = 0;
    for (const w of this.workloads.values()) {
      if (w.mode === "managed" && (w.state === "RUNNING" || w.state === "STARTING")) n++;
    }
    return n;
  }

  private queueEntries(): QueueEntry[] {
    return this.queueOrder
      .map((id) => this.workloads.get(id))
      .filter((w): w is MockWorkload => !!w && w.state === "QUEUED")
      .map((w, index) => ({
        workload_id: w.id,
        request_id: w.requestId,
        priority: w.request.priority,
        effective_priority: w.request.priority,
        queued_at_ms: this.opts.monotonicNow() - (this.queueOrder.length - index) * 100,
        wait_reason: w.queueReason,
      }));
  }

  // ------------------------------------------------------- workload controls

  async workloadCancel(params: CancelParams): Promise<CancelResult> {
    const w = this.requireWorkload(params.workload_id);
    w.cancelRequested = true;
    if (w.state === "QUEUED") {
      const i = this.queueOrder.indexOf(w.id);
      if (i >= 0) this.queueOrder.splice(i, 1);
      w.state = "CANCELLED";
      this.revision++;
      this.emit({ kind: "queue.changed", payload: { queue: this.queueEntries(), revision: this.revision } });
      this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
      this.scheduleFlush();
      return { state: w.state };
    }
    if (this.isLive(w.state)) {
      w.state = "STOPPING";
      this.revision++;
      this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
      this.scheduleFlush();
      this.schedule(() => {
        const session = w.sessionId ? this.sessions.get(w.sessionId) : undefined;
        if (session && !session.exited) {
          session.exited = true;
          this.emit({ kind: "session.exited", payload: { session_id: session.id, exit_code: null, descendants_remaining: false, reason: "cancelled" } });
        }
        w.state = "DRAINING";
        this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
        this.schedule(() => {
          w.state = "CANCELLED";
          w.exitCode = null;
          this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
          this.scheduleFlush();
        });
        this.scheduleFlush();
      });
    }
    return { state: w.state };
  }

  async workloadReprioritize(params: ReprioritizeParams): Promise<ReprioritizeResult> {
    const w = this.requireWorkload(params.workload_id);
    if (w.state !== "QUEUED") throw new RpcClientError("INVALID_STATE", "실행 중 작업에는 우선순위 변경을 적용하지 않습니다.");
    w.request = { ...w.request, priority: params.priority };
    this.revision++;
    const queue = this.queueEntries();
    this.emit({ kind: "queue.changed", payload: { queue, revision: this.revision } });
    this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
    this.scheduleFlush();
    return { queue };
  }

  async workloadUpdatePolicy(params: UpdatePolicyParams): Promise<UpdatePolicyResult> {
    const w = this.requireWorkload(params.workload_id);
    if (w.state !== "QUEUED") throw new RpcClientError("INVALID_STATE", "QUEUED에서만 정책을 변경할 수 있습니다.");
    w.request = { ...w.request, policy: params.policy };
    this.revision++;
    const summary = this.workloadSummary(w);
    this.emit({ kind: "workload.changed", payload: summary });
    this.scheduleFlush();
    return { workload: summary };
  }

  async workloadProcesses(_params: ProcessesParams): Promise<ProcessesResult> {
    return { processes: [], next_cursor: null };
  }

  // ----------------------------------------------------------------- session

  async sessionAttach(params: AttachParams): Promise<AttachResult> {
    const session = this.requireSession(params.session_id);
    if (session.views.size >= VIEWS_PER_SESSION) {
      throw new RpcClientError("SESSION_LIMIT", "세션당 최대 2개 view만 attach할 수 있습니다.");
    }
    const access = session.writerViewId && session.writerViewId !== params.view_id ? "reader" : params.access;
    session.views.set(params.view_id, { viewId: params.view_id, access });
    if (access === "writer") session.writerViewId = params.view_id;
    // attach마다 새 epoch; replay record에도 현재 epoch가 붙는다(02 §4).
    session.epoch = this.opts.uuid();
    // 스냅샷 복원(resume_from_seq): 저널 범위(1..=len+1) 안이면 그 seq부터 재생한다.
    const resume = params.resume_from_seq != null ? Number(params.resume_from_seq) : NaN;
    const replayFrom = Number.isSafeInteger(resume) && resume >= 1 && resume <= session.records.length + 1 ? resume : 1;
    session.ackedSeq = replayFrom - 1;
    session.lastDeliveredSeq = replayFrom - 1;
    session.unackedBytes = 0;
    session.blocked = false;
    const w = this.workloads.get(session.workloadId);
    if (w) this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
    this.scheduleFlush();
    return {
      epoch: session.epoch,
      replay_from_seq: String(replayFrom),
      last_seq: String(session.records.length),
      exited: session.exited,
      cols: session.cols,
      rows: session.rows,
    };
  }

  async sessionDetach(params: { session_id: string; view_id: string }): Promise<{ detached: true }> {
    const session = this.requireSession(params.session_id);
    session.views.delete(params.view_id);
    if (session.writerViewId === params.view_id) session.writerViewId = null;
    const w = this.workloads.get(session.workloadId);
    if (w) this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
    this.scheduleFlush();
    return { detached: true };
  }

  async sessionInput(params: InputParams): Promise<InputResult> {
    const session = this.requireSession(params.session_id);
    if (params.epoch !== session.epoch) {
      throw new RpcClientError("STALE_EPOCH", "이전 epoch의 입력은 거절됩니다.");
    }
    const view = session.views.get(this.writerViewOf(session) ?? "");
    if (!view || view.access !== "writer") {
      throw new RpcClientError("NOT_INPUT_OWNER", "읽기 전용 view는 입력을 보낼 수 없습니다.");
    }
    const bytes = base64ToBytes(params.data_b64);
    // Echo shell: 입력을 그대로 출력 journal에 되돀린다.
    this.appendRecord(session, { kind: "output", bytes });
    this.scheduleFlush();
    return { input_id: params.input_id, accepted_bytes: bytes.length };
  }

  async sessionResize(params: ResizeParams): Promise<ResizeResult> {
    const session = this.requireSession(params.session_id);
    if (params.epoch !== session.epoch) {
      throw new RpcClientError("STALE_EPOCH", "이전 epoch의 resize는 거절됩니다.");
    }
    if (params.cols < 2 || params.rows < 2 || params.cols > MAX_COLS_ROWS || params.rows > MAX_COLS_ROWS) {
      throw new RpcClientError("INVALID_ARGUMENT", "cols/rows는 2..1000 범위여야 합니다.");
    }
    session.cols = params.cols;
    session.rows = params.rows;
    const seq = this.appendRecord(session, {
      kind: "resize",
      bytes: null,
      cols: params.cols,
      rows: params.rows,
    });
    this.scheduleFlush();
    return { resize_id: params.resize_id, applied_seq: String(seq) };
  }

  sessionAck(ack: SessionAck): void {
    const session = this.sessions.get(ack.session_id);
    if (!session) return;
    if (ack.epoch !== session.epoch) return; // 이전 epoch ACK는 무시
    const through = Number(ack.through_seq);
    if (through <= session.ackedSeq) return; // 중복 ACK 무시
    if (through > session.lastDeliveredSeq) {
      const detail = `ACK through_seq=${through} exceeds last delivered seq=${session.lastDeliveredSeq}`;
      this.protocolErrors.push({ session_id: ack.session_id, detail });
      throw new RpcClientError("INVALID_ARGUMENT", "전송하지 않은 seq에 대한 ACK입니다.");
    }
    for (let seq = session.ackedSeq + 1; seq <= through; seq++) {
      const rec = session.records[seq - 1];
      session.unackedBytes -= rec?.bytes?.length ?? 0;
    }
    session.ackedSeq = through;
    if (session.blocked && session.unackedBytes <= this.opts.outputLowBytes) {
      session.blocked = false;
      this.emit({
        kind: "session.flow_blocked",
        payload: {
          session_id: session.id,
          blocked: false,
          view_id: session.writerViewId,
          unacked_bytes: String(session.unackedBytes),
        },
      });
      this.pumpSession(session);
      this.scheduleFlush();
    }
  }

  async sessionTakeControl(params: TakeControlParams): Promise<TakeControlResult> {
    const session = this.requireSession(params.session_id);
    if (session.writerViewId !== params.expected_owner) {
      throw new RpcClientError("INVALID_STATE", "소유자가 기대값과 다릅니다.");
    }
    const oldWriter = session.writerViewId;
    session.writerViewId = params.view_id;
    if (oldWriter && session.views.has(oldWriter)) {
      session.views.set(oldWriter, { viewId: oldWriter, access: "reader" });
    }
    session.epoch = this.opts.uuid();
    session.ackedSeq = 0;
    session.lastDeliveredSeq = 0;
    session.unackedBytes = 0;
    session.blocked = false;
    this.emit({
      kind: "session.owner_changed",
      payload: { session_id: session.id, epoch: session.epoch, owner_view_id: params.view_id },
    });
    this.scheduleFlush();
    return { epoch: session.epoch, owner_view_id: params.view_id };
  }

  /**
   * 08-pressure-relief §1: 이 창이 보고 있는 세션을 기록한다. mock은 제어
   * 연결 하나를 흉내 내므로 집계도 0~1건이다. 데몬처럼 모르는 세션과 이미
   * 끝난 세션은 INVALID_ARGUMENT로 거절한다(경합으로 방금 끝난 세션을
   * 보고하는 일이 정상 경로라, 호출부는 이 실패를 흡수한다).
   */
  async sessionFocus(params: SessionFocusParams): Promise<SessionFocusResult> {
    const sessionId = params.session_id ?? null;
    if (sessionId === null) {
      this.focusedSessionId = null;
      return { focused_session_ids: this.focusedSessionIds() };
    }
    const session = this.requireSession(sessionId);
    const workload = this.workloads.get(session.workloadId);
    if (session.exited || !workload || !this.isLive(workload.state)) {
      throw new RpcClientError("INVALID_ARGUMENT", "알 수 없는 세션입니다.");
    }
    this.focusedSessionId = session.id;
    return { focused_session_ids: this.focusedSessionIds() };
  }

  /** 끝난 세션은 집계에서 빠진다(데몬이 애초에 받아 주지 않는 값이다). */
  private focusedSessionIds(): string[] {
    const session = this.focusedSessionId ? this.sessions.get(this.focusedSessionId) : undefined;
    return session && !session.exited ? [session.id] : [];
  }

  /**
   * 08-pressure-relief §2의 수동 액션. mock에는 스케줄러가 없으므로 상태
   * 전이만 데몬과 같게 흉내 낸다: `yield`는 수동 양보(자동 복원 없음),
   * `restore`는 즉시 복원하고 지금 CPU 압력이 NORMAL이 아니면 이번 압력
   * 에피소드 동안 보호하며, `protect`는 보호 표시와 함께 복원하고,
   * `unprotect`는 표시만 지운다. 모르는·끝난 세션은 focus와 같은 자리에서
   * INVALID_ARGUMENT로 거절한다.
   */
  async sessionRelief(params: SessionReliefParams): Promise<SessionReliefResult> {
    const session = this.requireSession(params.session_id);
    const w = this.workloads.get(session.workloadId);
    if (session.exited || !w || !this.isLive(w.state)) {
      throw new RpcClientError("INVALID_ARGUMENT", "알 수 없는 세션입니다.");
    }
    switch (params.action) {
      case "yield":
        w.relief = {
          kind: "YIELDED",
          since_ms: String(this.opts.monotonicNow()),
          manual: true,
          partial: false,
        };
        break;
      case "restore":
        w.relief = { kind: "NONE" };
        // 압력이 남아 있으면 다음 틱에 다시 양보되지 않게 보호한다(§2).
        if (this.cpuPressure() !== "NORMAL") w.protected = true;
        break;
      case "protect":
        w.protected = true;
        w.relief = { kind: "NONE" };
        break;
      case "unprotect":
        w.protected = false;
        break;
    }
    // 완화가 바뀌면 snapshot revision이 오른다(구독자가 새로 읽는다).
    this.revision++;
    this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
    this.scheduleFlush();
    return { relief: { ...w.relief }, protected: w.protected };
  }

  /** 자동 양보 토글(08 §2) — 이미 양보된 세션은 이 호출로 복원되지 않는다. */
  async reliefSetPolicy(params: ReliefPolicyParams): Promise<ReliefPolicy> {
    this.reliefPolicy = { auto_yield: params.auto_yield };
    this.revision++;
    return { ...this.reliefPolicy };
  }

  /** 자원 가드 수동 일시정지/해제(08 §5). mock에는 가드 루프가 없으므로
   * 상태 전이만 흉내 낸다 — 수동 정지는 자동 재개 대상이 아니다. */
  async workloadSuspend(params: WorkloadSuspendParams): Promise<WorkloadGuardResult> {
    return this.workloadGuard(params, true);
  }

  async workloadResume(params: WorkloadSuspendParams): Promise<WorkloadGuardResult> {
    return this.workloadGuard(params, false);
  }

  private async workloadGuard(params: WorkloadSuspendParams, suspend: boolean): Promise<WorkloadGuardResult> {
    const w = this.workloads.get(params.workload_id);
    if (!w || !this.isLive(w.state)) {
      throw new RpcClientError("INVALID_STATE", "이미 끝난 작업입니다.");
    }
    const suspended = w.guard.kind === "SUSPENDED";
    if (suspend !== suspended) {
      w.guard = suspend
        ? { kind: "SUSPENDED", since_ms: String(this.opts.monotonicNow()), reason: "manual", manual: true, partial: false }
        : { kind: "NONE" };
      this.revision++;
      this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
      this.scheduleFlush();
    }
    return { guard: { ...w.guard } };
  }

  /** 자원 가드 정책 변경(08 §5). */
  async guardSetPolicy(params: { policy: GuardPolicy }): Promise<GuardPolicy> {
    this.guardPolicy = { ...params.policy };
    this.revision++;
    return { ...this.guardPolicy };
  }

  /** 마지막 host sample의 CPU 압력(아직 샘플이 없으면 NORMAL). */
  private cpuPressure(): PressureLevel {
    return this.hostSample?.cpu_pressure ?? "NORMAL";
  }

  /** 시험 헬퍼: 지금 host sample의 CPU 압력을 바꾼다(수동 복원 경로 검증용). */
  seedCpuPressure(level: PressureLevel): void {
    if (!this.hostSample) this.hostSample = this.nextHostSample();
    this.hostSample = { ...this.hostSample, cpu_pressure: level };
  }

  async retentionSetLimit(params: RetentionSetLimitParams): Promise<RetentionSetLimitResult> {
    const session = this.requireSession(params.session_id);
    session.retentionMaxBytes = params.max_bytes;
    return { max_bytes: params.max_bytes };
  }

  // 개입 링(W1-5): 시험이 pushIntervention으로 쌓고 interventionList가 읽는다.
  private interventionRing: InterventionNotice[] = [];

  /** 시험용 저널 라인 저장(W2 저널 검색). */
  private journalLines = new Map<string, Array<{ seq: number; text: string }>>();

  /** 시험 헬퍼: 세션 저널에 출력 줄을 심는다. */
  seedJournalLine(sessionId: string, seq: number, text: string): void {
    const list = this.journalLines.get(sessionId) ?? [];
    list.push({ seq, text });
    this.journalLines.set(sessionId, list);
  }

  async interventionList(): Promise<InterventionNotice[]> {
    return [...this.interventionRing];
  }

  /** 기록된 에이전트 세션(01 §6) — 시험이 seedAgentSessions로 심는다. */
  private agentSessions: AgentSessionRecord[] = [];

  /** 시험 헬퍼: 저장된 행을 통째로 갈아 끼운다(심는 순서는 상관없다). */
  seedAgentSessions(records: readonly AgentSessionRecord[]): void {
    this.agentSessions = records.map((record) => ({ ...record }));
  }

  /**
   * 데몬과 같은 순서·중복 제거(02 §8 / term-storage `list_agent_sessions`):
   * cwd로 먼저 걸러낸 뒤 (agent, agent_session_id)마다 last_seen_at(동률은
   * id) 기준 가장 최근 한 건만 남기고, last_seen_at DESC · id DESC로 정렬한
   * 다음 limit을 적용한다. 저장 순서를 그대로 돌려주면 순서에 의존하는
   * 버그가 시험에서 드러나지 않는다.
   */
  async agentSessionList(params: AgentSessionListParams = {}): Promise<AgentSessionRecord[]> {
    // 데몬과 같은 상한(term-contracts agent_session::limits::LIST_MAX = 200).
    const limit = Math.min(Math.max(params.limit ?? 50, 1), 200);
    const live = new Set(this.agentSessions.filter(r => r.active).map(r => `${r.agent}\t${r.agent_session_id}`));
    const newest = new Map<string, AgentSessionRecord>();
    for (const record of this.agentSessions) {
      if (params.cwd != null && record.cwd !== params.cwd) continue;
      if ((params.workload_id != null || params.pty_session_id != null) &&
        !(params.workload_id != null && record.workload_id === params.workload_id) &&
        !(params.pty_session_id != null && record.pty_session_id === params.pty_session_id)) continue;
      const key = `${record.agent}\t${record.agent_session_id}`;
      const seen = newest.get(key);
      if (!seen || compareAgentSessions(record, seen) < 0) newest.set(key, record);
    }
    return [...newest.values()]
      .sort(compareAgentSessions)
      .slice(0, limit)
      .map((record) => ({ ...record, active: live.has(`${record.agent}\t${record.agent_session_id}`) }));
  }

  async agentSessionForget(params: AgentSessionForgetParams): Promise<AgentSessionForgetResult> {
    const before = this.agentSessions.length;
    this.agentSessions = this.agentSessions.filter((record) => record.id !== params.id);
    return { forgotten: this.agentSessions.length < before };
  }

  async sessionSearch(params: SessionSearchParams): Promise<SessionSearchResult> {
    const needle = params.case_sensitive
      ? params.query
      : params.query.toLowerCase();
    const matches: SessionSearchMatch[] = [];
    let truncated = false;
    for (const [sessionId, lines] of this.journalLines) {
      if (params.session_id && params.session_id !== sessionId) continue;
      for (let i = lines.length - 1; i >= 0; i--) {
        const entry = lines[i];
        const haystack = params.case_sensitive ? entry.text : entry.text.toLowerCase();
        if (haystack.includes(needle)) {
          if (matches.length >= 50) {
            truncated = true;
            break;
          }
          matches.push({ session_id: sessionId, seq: String(entry.seq), line: entry.text.slice(0, 240) });
        }
      }
    }
    return { matches, truncated, sessions_scanned: this.journalLines.size };
  }

  // ---------------------------------------------------------- O1 mission RPCs
  //
  // 01-contracts.md §2·§3·§4가 정하는 순서를 그대로 따른다: request_id 멱등
  // (같은 지문 → 저장된 최초 응답, 다른 지문 → REQUEST_CONFLICT), CAS
  // (expected_revision 불일치 → REVISION_CONFLICT + details.current_revision),
  // 커밋 후 mission.changed hint 발행. revision과 event seq는 함께 1씩
  // 오른다(§1).

  async repositoryInspect(params: RepositoryInspectParams): Promise<RepositoryInspectResult> {
    let id = this.repositoryIds.get(params.path);
    if (!id) { id = this.opts.uuid(); this.repositoryIds.set(params.path, id); }
    const result: RepositoryInspectResult = {
      repository_id: id,
      canonical_path: params.path,
      head_oid: "b".repeat(40),
      clean: true,
      dirty_paths: [],
      verification_supported: true,
    };
    // missionCreate가 include_uncommitted일 때 base_snapshot.entry_count를 흉내 낼 수 있게 기억해 둔다.
    if (result.clean) this.dirtyEntryCounts.delete(params.path);
    else this.dirtyEntryCounts.set(params.path, result.dirty_paths.length);
    return result;
  }

  async missionCreate(params: MissionCreateParams): Promise<MutationResult> {
    const replay = this.replayStored<MutationResult>("mission.create", params.request_id, params);
    if (replay) return replay;
    let repositoryId = this.repositoryIds.get(params.repository_path);
    if (!repositoryId) {
      repositoryId = this.opts.uuid();
      this.repositoryIds.set(params.repository_path, repositoryId);
    }
    const followUpOf = params.follow_up_of ?? null;
    if (followUpOf !== null) {
      // 데몬 follow_up_base 흉내: 같은 저장소의 확정 완료 미션, base = 확정 후보 commit.
      const previous = this.missionStates.get(followUpOf);
      const accepted = previous?.mission;
      const candidateEntity = accepted?.candidate_id ? previous?.entities.get(accepted.candidate_id) : undefined;
      if (!accepted || accepted.state !== "completed" || accepted.accepted_at === null
        || accepted.repository_id !== repositoryId || !candidateEntity || candidateEntity.kind !== "candidate") {
        throw new RpcClientError("INVALID_ARGUMENT", "a follow-up requires a completed, accepted mission of the same repository", false,
          { reason_code: "follow_up_not_accepted" });
      }
      if (candidateEntity.value.commit_oid !== params.expected_base_oid) {
        throw new RpcClientError("INVALID_ARGUMENT", "the base must be the previous mission's accepted result commit", false,
          { reason_code: "follow_up_base_mismatch" });
      }
    }
    if (params.include_uncommitted === true && followUpOf !== null) {
      throw new RpcClientError("INVALID_ARGUMENT", "include_uncommitted cannot be combined with follow_up_of", false,
        { reason_code: "follow_up_snapshot" });
    }
    // include_uncommitted 흉내: repositoryInspect가 기억해 둔 더러운 경로 개수(없으면 1)로
    // base_snapshot을 채우고, base_oid는 HEAD와 구별되는 가짜 스냅샷 commit으로 바꾼다.
    const baseSnapshot: BaseSnapshot | null = params.include_uncommitted === true
      ? { head_oid: params.expected_base_oid, entry_count: this.dirtyEntryCounts.get(params.repository_path) ?? 1 }
      : null;
    const baseOid = baseSnapshot ? fakeSnapshotOid(`${params.repository_path}:${params.expected_base_oid}`) : params.expected_base_oid;
    const mission: Mission = {
      id: this.opts.uuid(),
      // 생성 transaction이 아래에서 1로 올린다(§1: revision·seq는 1에서 시작).
      revision: "0",
      state: "draft",
      phase: "planning",
      title: params.title,
      repository_path: params.repository_path,
      repository_id: repositoryId,
      base_oid: baseOid,
      goal_ref: { ...params.goal_ref },
      requirements: params.requirements.map((r) => ({ ...r })),
      policy: cloneJson(params.policy),
      role_bindings: params.role_bindings.map((b) => ({ ...b })),
      plan_revision: 0,
      candidate_id: null,
      open_decision_count: 0,
      active_time_ms: "0",
      automatic_start_count: 0,
      created_at: new Date().toISOString(),
      updated_at: new Date().toISOString(),
      archived_at: null,
      accepted_at: null,
      failure_code: null,
      follow_up_of: followUpOf,
      ...(baseSnapshot ? { base_snapshot: baseSnapshot } : {}),
    };
    const state: MockMissionState = { mission, entities: new Map(), events: [] };
    this.missionStates.set(mission.id, state);
    const result = this.commitMissionTransaction(state, "created", [
      { entity_kind: "mission", entity_id: mission.id, operation: "upsert" },
    ]);
    this.requests.set(params.request_id, {
      fingerprint: fingerprintOf("mission.create", params),
      result: cloneJson(result),
    });
    return result;
  }

  async missionList(params: MissionListParams): Promise<MissionListResult> {
    const start = params.cursor ? this.parseCursor(params.cursor) : 0;
    const all = [...this.missionStates.values()]
      .map((s) => s.mission)
      .filter((m) => (params.archived ? m.archived_at !== null : m.archived_at === null))
      .sort((a, b) =>
        a.created_at !== b.created_at
          ? a.created_at < b.created_at
            ? 1
            : -1
          : a.id < b.id
            ? 1
            : -1,
      );
    const limit = Math.min(Math.max(params.limit || 50, 1), 200);
    const items = all.slice(start, start + limit).map(cloneJson);
    return { items, next_cursor: start + items.length < all.length ? String(start + items.length) : null };
  }

  async missionSnapshot(params: MissionSnapshotParams): Promise<SnapshotPage> {
    const state = this.requireMission(params.mission_id);
    let snapshot: MockSnapshot | undefined = params.snapshot_id
      ? this.snapshots.get(params.snapshot_id)
      : undefined;
    if (params.snapshot_id) {
      if (!snapshot) {
        throw new RpcClientError("SNAPSHOT_EXPIRED", "스냅샷이 만료되었거나 존재하지 않습니다.");
      }
      // 커서는 스냅샷·미션에 binding된다(§4).
      if (snapshot.missionId !== params.mission_id) {
        throw new RpcClientError("INVALID_ARGUMENT", "다른 미션의 snapshot_id/cursor는 사용할 수 없습니다.");
      }
      if (Date.now() > snapshot.expiresAtMs) {
        this.snapshots.delete(snapshot.snapshotId);
        throw new RpcClientError("SNAPSHOT_EXPIRED", "스냅샷이 만료되었습니다.");
      }
    } else {
      // 신규 materialize: 미션당 최대 2개, 60초(§4). 오래된 것부터 내쫓는다.
      const owned = [...this.snapshots.values()].filter((s) => s.missionId === params.mission_id);
      for (let i = 0; i <= owned.length - SNAPSHOTS_PER_MISSION; i++) {
        this.snapshots.delete(owned[i].snapshotId);
      }
      snapshot = {
        snapshotId: this.opts.uuid(),
        missionId: params.mission_id,
        entities: [...state.entities.values()],
        atSeq: String(state.events.length),
        revision: state.mission.revision,
        expiresAtMs: Date.now() + SNAPSHOT_TTL_MS,
      };
      this.snapshots.set(snapshot.snapshotId, snapshot);
    }
    const start = params.cursor ? this.parseCursor(params.cursor) : 0;
    if (start > snapshot.entities.length) {
      throw new RpcClientError("INVALID_ARGUMENT", "cursor가 스냅샷 범위를 벗어났습니다.");
    }
    const page = snapshot.entities.slice(start, start + SNAPSHOT_PAGE_ENTITIES);
    const end = start + page.length;
    return {
      snapshot_id: snapshot.snapshotId,
      mission_id: snapshot.missionId,
      at_seq: snapshot.atSeq,
      revision: snapshot.revision,
      entities: page,
      next_cursor: end < snapshot.entities.length ? String(end) : null,
      expires_at: new Date(snapshot.expiresAtMs).toISOString(),
    };
  }

  async missionEvents(params: MissionEventsParams): Promise<MissionEventsResult> {
    const state = this.requireMission(params.mission_id);
    const after = this.parseU64(params.after_seq, "after_seq");
    const limit = Math.min(Math.max(params.limit || 50, 1), 200);
    const events = state.events.filter((e) => Number(e.seq) > after).slice(0, limit);
    const lastSeq = events.length > 0 ? Number(events[events.length - 1].seq) : after;
    return {
      events: events.map(cloneJson),
      high_watermark: String(state.events.length),
      next_after_seq: String(lastSeq),
    };
  }

  async missionControl(params: MissionControlParams): Promise<MutationResult> {
    return this.missionMutation(
      "mission.control",
      params,
      params.action === "archive" || params.action === "unarchive" ? "archived" : "changed",
      (state) => {
        const m = { ...state.mission };
        switch (params.action) {
          case "start":
            if (m.state !== "draft" && m.state !== "paused") {
              throw new RpcClientError("INVALID_STATE", "draft/paused 미션만 start할 수 있습니다.");
            }
            m.state = "running";
            break;
          case "pause":
            if (m.state !== "running") {
              throw new RpcClientError("INVALID_STATE", "running 미션만 pause할 수 있습니다.");
            }
            m.state = "paused";
            break;
          case "resume":
            if (m.state !== "paused") {
              throw new RpcClientError("INVALID_STATE", "paused 미션만 resume할 수 있습니다.");
            }
            m.state = "running";
            break;
          case "cancel":
            if (isTerminalMissionState(m.state)) {
              throw new RpcClientError("INVALID_STATE", "이미 종료된 미션입니다.");
            }
            m.state = "cancelled";
            break;
          case "archive":
            // archive는 분류 변경이지 삭제가 아니다(§6). 활성 미션은 거절.
            if (!isTerminalMissionState(m.state)) {
              throw new RpcClientError("INVALID_STATE", "활성 미션은 archive할 수 없습니다.");
            }
            m.archived_at = new Date().toISOString();
            break;
          case "unarchive":
            if (m.archived_at === null) {
              throw new RpcClientError("INVALID_STATE", "archive되지 않은 미션입니다.");
            }
            m.archived_at = null;
            break;
        }
        state.mission = m;
        return [{ entity_kind: "mission", entity_id: m.id, operation: "upsert" }];
      },
    );
  }

  async missionAccept(params: MissionAcceptParams): Promise<MutationResult> {
    return this.missionMutation("mission.accept", params, "accepted", (state) => {
      const candidate = state.entities.get(params.candidate_id);
      if (!candidate || candidate.kind !== "candidate") {
        throw new RpcClientError("STALE_CANDIDATE", "후보가 존재하지 않거나 만료되었습니다.");
      }
      const acknowledgements = params.acknowledged_reconciled_run_ids ?? [];
      if ([...state.entities.values()].some(e => e.kind === "task" && e.value.required && e.value.state === "cancelled")) {
        throw new RpcClientError("INVALID_STATE", "Cancelled required tasks need retry or a replacement plan");
      }
      const uncertain = [...state.entities.values()].flatMap(e => e.kind === "run" && ["unknown", "interrupted"].includes(e.value.state) ? [e.value] : []);
      if (new Set(acknowledgements).size !== acknowledgements.length || acknowledgements.some(id => !uncertain.some(r => r.id === id))) {
        throw new RpcClientError("INVALID_ARGUMENT", "Invalid reconciled run acknowledgement");
      }
      if (uncertain.some(run => !run.reconciliation_ref || !acknowledgements.includes(run.id))) {
        throw new RpcClientError("INVALID_STATE", "Unknown execution requires termination evidence and explicit review");
      }
      const m = { ...state.mission };
      m.candidate_id = params.candidate_id;
      m.state = "completed";
      m.phase = "done";
      m.accepted_at = new Date().toISOString();
      state.mission = m;
      return [
        { entity_kind: "mission", entity_id: m.id, operation: "upsert" },
        { entity_kind: "candidate", entity_id: params.candidate_id, operation: "upsert" },
      ];
    });
  }

  async missionMessage(params: MissionMessageParams): Promise<MutationResult> {
    return this.missionMutation("mission.message", params, "changed", (state) => {
      if (["stopping", "completed", "failed", "cancelled"].includes(state.mission.state)) throw new RpcClientError("INVALID_STATE", "Mission is closed");
      if (params.supersedes_message_id) {
        const source = state.entities.get(params.supersedes_message_id);
        if (!source || source.kind !== "message") throw new RpcClientError("NOT_FOUND", "Original message is missing");
        const original = source.value;
        const entities = [...state.entities.values()];
        if (original.role !== "user" || !["unknown", "rejected"].includes(original.delivery) || original.target_task_id !== params.target_task_id
          || entities.some(e => e.kind === "message" && e.value.supersedes_message_id === original.id)
          || entities.some(e => e.kind === "decision" && e.value.answer_message_id === original.id)) {
          throw new RpcClientError("INVALID_STATE", "Instruction cannot be replaced");
        }
      }
      if (params.target_task_id) {
        const task = state.entities.get(params.target_task_id);
        if (!task || task.kind !== "task") {
          throw new RpcClientError("NOT_FOUND", "알 수 없는 대상 task입니다.");
        }
        if (task.value.kind === "verify" || isDeterministicIntegration(task.value) || ["succeeded", "failed", "cancelled", "superseded"].includes(task.value.state)) throw new RpcClientError("INVALID_STATE", "Task cannot receive instructions");
      }
      const message: Message = {
        id: this.opts.uuid(),
        mission_id: state.mission.id,
        target_task_id: params.target_task_id,
        role: "user",
        run_id: null,
        body_ref: { ...params.body_ref },
        delivery: params.supersedes_message_id ? "queued" : "delivered",
        supersedes_message_id: params.supersedes_message_id ?? null,
        created_at: new Date().toISOString(),
      };
      state.entities.set(message.id, { kind: "message", value: message });
      return [{ entity_kind: "message", entity_id: message.id, operation: "upsert" }];
    });
  }

  async missionPlanApply(params: MissionPlanApplyParams): Promise<MutationResult> {
    return this.missionMutation("mission.plan.apply", params, "plan_applied", (state) => {
      const m = { ...state.mission };
      m.plan_revision += 1;
      if (m.phase === "planning") m.phase = "implementing";
      state.mission = m;
      return [{ entity_kind: "mission", entity_id: m.id, operation: "upsert" }];
    });
  }

  async missionTaskControl(params: MissionTaskControlParams): Promise<MutationResult> {
    return this.missionMutation("mission.task.control", params, "changed", (state) => {
      const current = state.entities.get(params.task_id);
      if (!current || current.kind !== "task") {
        throw new RpcClientError("NOT_FOUND", "알 수 없는 task입니다.");
      }
      let task = { ...current.value };
      if (isDeterministicIntegration(task) && task.integration == null
        && (params.action === "reassign" || params.binding_id != null)) {
        throw new RpcClientError("POLICY_DENIED", "Automatic integration does not use a model binding");
      }
      switch (params.action) {
        case "cancel":
          task = { ...task, state: "cancelled" };
          break;
        case "retry":
          if (task.state === "cancelled" && (task.active_run_id !== null || !taskRunsInPhase(task, state.mission))) {
            throw new RpcClientError("INVALID_STATE", "Cancelled task cannot retry until it ends in the current phase");
          }
          task = { ...task, state: "ready", binding_id: params.binding_id ?? task.binding_id, blocked_code: null };
          break;
        case "reassign":
          if (!params.binding_id) {
            throw new RpcClientError("INVALID_ARGUMENT", "reassign에는 binding_id가 필요합니다.");
          }
          task = { ...task, binding_id: params.binding_id };
          break;
      }
      state.entities.set(task.id, { kind: "task", value: task });
      return [{ entity_kind: "task", entity_id: task.id, operation: "upsert" }];
    });
  }

  async missionPolicyUpdate(params: MissionPolicyUpdateParams): Promise<MutationResult> {
    return this.missionMutation("mission.policy.update", params, "changed", (state) => {
      // 사용자 control 연결만, running/paused/draft에서 허용(§3).
      if (
        state.mission.state !== "running" &&
        state.mission.state !== "paused" &&
        state.mission.state !== "draft"
      ) {
        throw new RpcClientError("INVALID_STATE", "running/paused/draft 미션에서만 정책을 바꿀 수 있습니다.");
      }
      state.mission = {
        ...state.mission,
        policy: cloneJson(params.policy),
        role_bindings: params.role_bindings.map((b) => ({ ...b })),
      };
      return [{ entity_kind: "mission", entity_id: state.mission.id, operation: "upsert" }];
    });
  }

  async missionFindingResolve(params: MissionFindingResolveParams): Promise<MutationResult> {
    return this.missionMutation("mission.finding.resolve", params, "changed", (state) => {
      const current = state.entities.get(params.finding_id);
      if (!current || current.kind !== "finding") {
        throw new RpcClientError("NOT_FOUND", "알 수 없는 finding입니다.");
      }
      if (current.value.resolution !== "open") {
        throw new RpcClientError("INVALID_STATE", "이미 처리된 finding입니다.");
      }
      state.entities.set(params.finding_id, {
        kind: "finding",
        value: {
          ...current.value,
          resolution: params.resolution,
          resolution_ref: { ...params.reason_ref },
        },
      });
      return [{ entity_kind: "finding", entity_id: params.finding_id, operation: "upsert" }];
    });
  }

  async missionDecisionAnswer(params: MissionDecisionAnswerParams): Promise<MutationResult> {
    return this.missionMutation("mission.decision.answer", params, "decision_answered", (state) => {
      const current = state.entities.get(params.decision_id);
      if (!current || current.kind !== "decision") {
        throw new RpcClientError("NOT_FOUND", "알 수 없는 decision입니다.");
      }
      if (current.value.state !== "open") {
        // 지금 답을 기다리는 질문으로 안내한다(§7 STALE_DECISION).
        const open = [...state.entities.values()].find(
          (e): e is { kind: "decision"; value: Decision } => e.kind === "decision" && e.value.state === "open",
        );
        throw new RpcClientError("STALE_DECISION", "이미 답변된 질문입니다.", false,
          open ? { decision_id: open.value.id } : undefined);
      }
      const extra: Change[] = [];
      if (current.value.kind === "recovery" && current.value.options.some(o => o.id === "stop_reconciled_mission")) {
        const run = current.value.requesting_run_id ? state.entities.get(current.value.requesting_run_id) : null;
        const task = run && run.kind === "run" ? state.entities.get(run.value.task_id) : null;
        if (task && task.kind === "task" && task.value.integration) {
          const exec = run && run.kind === "run" && run.value.exec_id ? state.entities.get(run.value.exec_id) : null;
          if (!run || run.kind !== "run" || !["unknown", "interrupted"].includes(run.value.state) || !run.value.reconciliation_ref
            || !exec || exec.kind !== "exec" || exec.value.state !== "exited" || !exec.value.ended_at
            || task.value.state !== "blocked" || task.value.active_run_id !== null || task.value.blocked_code !== "outcome_unknown_ended") {
            throw new RpcClientError("STALE_DECISION", "Integration has no current termination evidence");
          }
          if (params.option_id === "retry_reconciled_task") {
            if (task.value.attempt_count >= state.mission.policy.max_attempts_per_task) throw new RpcClientError("POLICY_DENIED", "Attempt budget exhausted");
            state.entities.set(task.value.id, { kind: "task", value: { ...task.value, state: "ready", blocked_code: null, workspace_id: null,
              contract: { ...task.value.contract, objective_ref: task.value.integration.plan_ref },
              integration: { ...task.value.integration, step: "automatic" } } });
            extra.push({ entity_kind: "task", entity_id: task.value.id, operation: "upsert" });
          } else if (params.option_id === "stop_reconciled_mission") {
            state.mission = { ...state.mission, state: "stopping" };
          } else {
            throw new RpcClientError("INVALID_ARGUMENT", "Choose an explicit integration recovery action");
          }
        }
      }
      if (current.value.kind === "conflict" && current.value.requesting_run_id) {
        const run = state.entities.get(current.value.requesting_run_id);
        const task = run && run.kind === "run" ? state.entities.get(run.value.task_id) : null;
        if (!task || task.kind !== "task" || !task.value.integration || task.value.state !== "failed") {
          throw new RpcClientError("INVALID_STATE", "Conflict no longer owns this integration task");
        }
        if (params.option_id === "resolve_and_reintegrate") {
          // 빠른 모드(Integrator 없음)에서는 데몬이 이 선택지를 내지 않는다.
          if (!current.value.options.some(o => o.id === "resolve_and_reintegrate")) {
            throw new RpcClientError("INVALID_ARGUMENT", "this mission has no Integrator to resolve the conflict", false,
              { reason_code: "option_invalid" });
          }
          state.entities.set(task.value.id, { kind: "task", value: { ...task.value, state: "ready", blocked_code: null,
            integration: { ...task.value.integration, step: { resolving: { conflict_run_id: current.value.requesting_run_id } } } } });
          extra.push({ entity_kind: "task", entity_id: task.value.id, operation: "upsert" });
        } else if (params.option_id === "stop_mission") {
          state.mission = { ...state.mission, state: "stopping" };
        } else if (params.option_id === "exclude_candidate") {
          const tasks = [...state.entities.values()].flatMap(e => e.kind === "task" ? [e.value] : []);
          const cycle = Math.max(0, ...tasks.map(t => t.repair_cycle)) + 1;
          const binding = state.mission.role_bindings.find(b => b.role === "lead")?.primary_binding_id;
          if (!binding || cycle > state.mission.policy.max_repair_cycles) throw new RpcClientError("PLAN_LIMIT", "Configure a Lead and allow another repair cycle");
          const id = this.opts.uuid();
          state.entities.set(task.value.id, { kind: "task", value: { ...task.value, state: "superseded" } });
          state.entities.set(id, { kind: "task", value: { ...task.value, id, title: "Replan after excluding conflicting input", kind: "plan", role: "lead",
            required: true, state: "planned", integration: null, active_run_id: null, attempt_count: 0, workspace_id: null,
            binding_id: binding, ordinal: Math.max(0, ...tasks.map(t => t.ordinal)) + 1, repair_cycle: cycle,
            blocked_code: null, dispatch_after_unix_ms: null, depends_on: [], parent_task_id: null, replacement_of: null,
            contract: { ...task.value.contract, objective_ref: state.mission.goal_ref, allowed_paths: [], input_artifact_ids: [], expected_outputs: ["report"] } } });
          state.mission = { ...state.mission, phase: "planning", candidate_id: null };
          extra.push({ entity_kind: "task", entity_id: task.value.id, operation: "upsert" }, { entity_kind: "task", entity_id: id, operation: "upsert" });
        } else {
          throw new RpcClientError("INVALID_ARGUMENT", "Choose an integrator resolution or stop action");
        }
      }
      state.entities.set(params.decision_id, {
        kind: "decision",
        value: {
          ...current.value,
          state: "answered",
          selected_option_id: params.option_id,
          answer_ref: params.answer_ref ? { ...params.answer_ref } : null,
          answered_at: new Date().toISOString(),
        },
      });
      state.mission = {
        ...state.mission,
        open_decision_count: Math.max(0, state.mission.open_decision_count - 1),
      };
      return [
        { entity_kind: "decision", entity_id: params.decision_id, operation: "upsert" },
        { entity_kind: "mission", entity_id: state.mission.id, operation: "upsert" },
        ...extra,
      ];
    });
  }

  async missionRequestGet(params: MissionRequestGetParams): Promise<MissionRequestGetResult> {
    // 타임아웃 복구 경로(§2): 저장된 최초 응답을 그대로 돌려준다.
    const entry = this.requests.get(params.request_id);
    if (entry && isMutationResult(entry.result)) {
      return { state: "committed", result: cloneJson(entry.result) };
    }
    return { state: "not_found", result: null };
  }

  async missionActivity(params: MissionActivityParams): Promise<MissionActivityResult> {
    this.requireMission(params.mission_id);
    // mock은 adapter를 돌리지 않으므로 spool은 항상 비어 있다(§4).
    return { body_ref: null, next_offset: params.after_offset, complete: true };
  }

  async missionRunAttestExited(params: MissionRunAttestExitedParams): Promise<MutationResult> {
    if (!/^[a-z0-9_]{1,64}$/.test(params.attestation)) {
      throw new RpcClientError("INVALID_ARGUMENT", "attestation must be a statement key [a-z0-9_]{1,64}");
    }
    return this.missionMutation("mission.run.attest_exited", params, "reconciled", (state) => {
      const current = state.entities.get(params.run_id);
      if (!current || current.kind !== "run") {
        throw new RpcClientError("NOT_FOUND", "알 수 없는 run입니다.");
      }
      const run = current.value;
      const exec = run.exec_id ? state.entities.get(run.exec_id) : undefined;
      // 데몬 attestation_target 흉내: 종료 증거 없는 unknown/interrupted만, 관측된 종료는 자동 정합 대상.
      if (!["unknown", "interrupted"].includes(run.state) || run.reconciliation_ref !== null
        || (exec !== undefined && exec.kind === "exec" && exec.value.state === "exited")) {
        throw new RpcClientError("INVALID_STATE", "only an unknown or interrupted run without termination evidence can be confirmed", false,
          { reason_code: "attestation_not_applicable" });
      }
      const now = new Date().toISOString();
      const changes: Change[] = [];
      state.entities.set(run.id, { kind: "run", value: {
        ...run,
        reconciliation_ref: { id: this.opts.uuid(), sha256: "d".repeat(64), bytes: "256", media_type: "application/json" },
        reconciliation_kind: "user_attested",
      } });
      changes.push({ entity_kind: "run", entity_id: run.id, operation: "upsert" });
      if (exec !== undefined && exec.kind === "exec") {
        state.entities.set(exec.value.id, { kind: "exec", value: { ...exec.value, state: "exited", ended_at: now, exit_code: null } });
        changes.push({ entity_kind: "exec", entity_id: exec.value.id, operation: "upsert" });
      }
      const workspace = run.workspace_id ? state.entities.get(run.workspace_id) : undefined;
      if (workspace !== undefined && workspace.kind === "workspace" && workspace.value.writer_run_id === run.id) {
        state.entities.set(workspace.value.id, { kind: "workspace", value: { ...workspace.value, writer_run_id: null, state: "quarantined" } });
        changes.push({ entity_kind: "workspace", entity_id: workspace.value.id, operation: "upsert" });
      }
      const task = state.entities.get(run.task_id);
      if (task !== undefined && task.kind === "task" && task.value.active_run_id === run.id) {
        const terminal = ["succeeded", "failed", "cancelled", "superseded"].includes(task.value.state);
        state.entities.set(task.value.id, { kind: "task", value: {
          ...task.value,
          active_run_id: null,
          workspace_id: null,
          ...(terminal ? {} : { state: "blocked" as const, blocked_code: "outcome_unknown_ended", dispatch_after_unix_ms: null }),
          updated_at: now,
        } });
        changes.push({ entity_kind: "task", entity_id: task.value.id, operation: "upsert" });
      }
      return [{ entity_kind: "mission", entity_id: state.mission.id, operation: "upsert" }, ...changes];
    });
  }

  async workspaceUsage(params: WorkspaceUsageParams): Promise<WorkspaceUsageResult> {
    const states = params.mission_id !== null
      ? [this.requireMission(params.mission_id)]
      : [...this.missionStates.values()];
    const missions: WorkspaceUsageEntry[] = [];
    for (const state of states) {
      const entry = this.workspaceUsageEntry(state);
      if (params.mission_id !== null || entry.workspaces > 0) missions.push(entry);
    }
    const total = missions.reduce((sum, m) => sum + BigInt(m.bytes), 0n);
    return { missions, total_bytes: total.toString() };
  }

  async workspaceCleanup(params: WorkspaceCleanupParams): Promise<WorkspaceCleanupResult> {
    const replay = this.replayStored<WorkspaceCleanupResult>("workspace.cleanup", params.request_id, params);
    if (replay) return replay;
    const state = this.requireMission(params.mission_id);
    const blocked = this.workspaceBlockedReason(state);
    if (blocked !== null) {
      throw new RpcClientError("INVALID_STATE", "workspaces of this mission cannot be cleaned up yet", false, { reason_code: blocked });
    }
    const kept: WorkspaceKept[] = [];
    let removed = 0;
    for (const workspace of this.onDiskWorkspaces(state)) {
      // 결정적 규칙: writer lease가 남으면 사용 중, 격리(quarantined)는 변경이 남은 것으로 보존.
      if (workspace.writer_run_id !== null) {
        kept.push({ path: workspace.path, reason: "run_active" });
      } else if (workspace.state === "quarantined") {
        kept.push({ path: workspace.path, reason: "dirty" });
      } else {
        this.removedWorkspaces.add(workspace.id);
        removed += 1;
      }
    }
    const result: WorkspaceCleanupResult = {
      removed,
      freed_bytes: (BigInt(removed) * BigInt(MOCK_WORKSPACE_BYTES)).toString(),
      kept,
    };
    this.requests.set(params.request_id, {
      fingerprint: fingerprintOf("workspace.cleanup", params),
      result: cloneJson(result),
    });
    return cloneJson(result);
  }

  private onDiskWorkspaces(state: MockMissionState) {
    return [...state.entities.values()].flatMap((e) =>
      e.kind === "workspace" && e.value.owned_by_daemon && !this.removedWorkspaces.has(e.value.id) ? [e.value] : [],
    );
  }

  private workspaceBlockedReason(state: MockMissionState): string | null {
    if (!isTerminalMissionState(state.mission.state)) return "mission_active";
    const holdsSlot = [...state.entities.values()].some((e) =>
      e.kind === "run" && (["prepared", "starting", "running", "awaiting_input", "stopping"].includes(e.value.state)
        || (["unknown", "interrupted"].includes(e.value.state) && e.value.reconciliation_ref === null)));
    return holdsSlot ? "run_unreconciled" : null;
  }

  private workspaceUsageEntry(state: MockMissionState): WorkspaceUsageEntry {
    const workspaces = this.onDiskWorkspaces(state).length;
    const blocked = this.workspaceBlockedReason(state);
    return {
      mission_id: state.mission.id,
      workspaces,
      bytes: (BigInt(workspaces) * BigInt(MOCK_WORKSPACE_BYTES)).toString(),
      cleanable: blocked === null && workspaces > 0,
      blocked_reason: blocked,
    };
  }

  async bindingList(): Promise<BindingListResult> {
    return { bindings: [...this.bindings.values()].map(cloneJson) };
  }

  async bindingSave(params: BindingSaveParams): Promise<BindingSaveResult> {
    // 로컬 증거는 데몬 소유다(11 §7): 보내온 값은 버리고 저장된 문서의 것을 이어 간다.
    const document: Binding = { ...cloneJson(params.binding), local_evidence: mockCarriedEvidence(this.bindings.get(params.binding.id), params.binding) };
    const saved = this.casSave("binding.save", params, this.bindings, document);
    return { binding: cloneJson(saved) };
  }

  async bindingProbe(params: BindingProbeParams): Promise<BindingProbeResult> {
    const binding = this.bindings.get(params.binding_id);
    if (!binding) throw new RpcClientError("NOT_FOUND", "알 수 없는 binding입니다.");
    // 유료 추론 없이 메타데이터 확인 + 무추론 자가 진단만 한다(§3, 11 §6).
    const now = new Date().toISOString();
    const checked: Binding = { ...binding, revision: (BigInt(binding.revision) + 1n).toString(), checked_at: now };
    const observed = mockInstalledVersion(binding.runtime);
    const routed = binding.enabled && binding.runtime !== "fake" && mockAdapterSupportsRoute(binding);
    // 데몬 registry 흉내(11 §4): 출시 증거 → 이 PC의 자가 진단 → 사용자 동의 순으로 처음
    // supported인 층을 쓴다. 어느 층도 없으면 저장된 값 그대로.
    if (
      binding.runtime === "codex" &&
      binding.provider_id === "openai" &&
      binding.auth_route === "subscription" &&
      binding.credential_ref === null &&
      binding.endpoint_ref === null &&
      binding.model_id === MOCK_CODEX_PROVEN_MODEL &&
      binding.enabled
    ) {
      const verified = { supported: true, reason_code: null };
      checked.runtime_version = MOCK_CODEX_VERSION;
      checked.capabilities = {
        ...binding.capabilities,
        structured_result: verified,
        events: verified,
        cancel: verified,
        read_only: verified,
        scoped_write: verified,
      };
      checked.local_evidence = mockProbeEvidence(binding, MOCK_CODEX_VERSION, mockLocalProbe(binding), now);
    } else if (observed !== null && routed) {
      // 이 PC의 자가 진단이 증명한 기능(11 §3.1 `local_probe`) — 출시 증거가 없는 버전도
      // 동의 없이 열린다. 어댑터가 구현하지 않은 기능은 adapter_not_implemented.
      const implemented = mockImplementedCapabilities(binding.runtime);
      const capabilities = { ...binding.capabilities };
      for (const key of Object.keys(capabilities) as Array<keyof Binding["capabilities"]>) {
        capabilities[key] = implemented.includes(key)
          ? { supported: true, reason_code: "local_probe" }
          : { supported: false, reason_code: "adapter_not_implemented" };
      }
      checked.runtime_version = observed;
      checked.capabilities = capabilities;
      checked.local_evidence = mockProbeEvidence(binding, observed, mockLocalProbe(binding), now);
    } else if (routed && binding.experimental_version) {
      // 동의는 연결당 한 번이다(11 §3.4): 관측 버전과 같아야 한다는 조건은 없어졌다.
      const implemented = mockImplementedCapabilities(binding.runtime);
      const capabilities = { ...binding.capabilities };
      for (const key of Object.keys(capabilities) as Array<keyof Binding["capabilities"]>) {
        capabilities[key] = implemented.includes(key)
          ? { supported: true, reason_code: "experimental_opt_in" }
          : { supported: false, reason_code: "adapter_not_implemented" };
      }
      if (observed !== null) checked.runtime_version = observed;
      checked.capabilities = capabilities;
    }
    this.bindings.set(binding.id, checked);
    return { binding: cloneJson(checked), models: mockAdvertisedModels(binding.runtime, binding.provider_id), installation: "verified" };
  }

  async runtimeDetect(): Promise<RuntimeDetectResult> {
    // 읽기 전용 감지(§3): 아무것도 저장하지 않는다. 결정적 fixture —
    // codex는 macOS 출시 증거로 네 역할 검증, claude는 출시 증거는 없지만 이 PC의
    // 자가 진단이 네 역할을 열었고(11 §7 `verified_locally`), opencode는 찾지 못함.
    return {
      runtimes: [
        {
          runtime: "codex",
          program: "/opt/homebrew/bin/codex",
          version: MOCK_CODEX_VERSION,
          installation: "verified",
          login: "found",
          configured_model_id: MOCK_CODEX_PROVEN_MODEL,
          suggested_provider_id: "openai",
          proven_model_id: MOCK_CODEX_PROVEN_MODEL,
          models: mockAdvertisedModels("codex"),
          verified_roles: ["lead", "builder", "reviewer", "integrator"],
          grade: "verified",
          experimental_roles: ["lead", "builder", "reviewer", "integrator"],
        },
        {
          // 이 버전의 출시 증거는 없지만 값싼 자가 진단(local_probe::run_cheap)이 통과해
          // 동의 없이 네 역할을 쓸 수 있다.
          runtime: "claude",
          program: "/opt/homebrew/bin/claude",
          version: MOCK_CLAUDE_VERSION,
          installation: "verified",
          login: "found",
          configured_model_id: MOCK_CLAUDE_MODEL,
          suggested_provider_id: "anthropic",
          proven_model_id: null,
          models: mockAdvertisedModels("claude"),
          alt_models: [{ provider_id: "zai-coding-plan", models: MOCK_CLAUDE_ZAI_MODELS.map((id) => ({ id, efforts: [] })) }],
          verified_roles: [],
          grade: "verified_locally",
          experimental_roles: ["lead", "builder", "reviewer", "integrator"],
        },
        {
          runtime: "opencode",
          program: "",
          version: null,
          installation: "not_found",
          login: "unknown",
          configured_model_id: null,
          suggested_provider_id: "zai-coding-plan",
          proven_model_id: null,
          models: mockAdvertisedModels("opencode"),
          verified_roles: [],
          grade: "not_installed",
          experimental_roles: [],
        },
      ],
    };
  }

  async templateList(params: TemplateListParams): Promise<TemplateListResult> {
    const templates = [...this.templates.values()].filter(
      (t) => !params.repository_id || t.repository_id === params.repository_id,
    );
    return { templates: templates.map(cloneJson) };
  }

  async templateSave(params: TemplateSaveParams): Promise<TemplateSaveResult> {
    const saved = this.casSave("template.save", params, this.templates, cloneJson(params.template));
    return { template: cloneJson(saved) };
  }

  async verificationList(params: VerificationListParams): Promise<VerificationListResult> {
    const commands = [...this.verificationCommands.values()].filter(
      (c) => c.repository_id === params.repository_id,
    );
    return { commands: commands.map(cloneJson) };
  }

  async verificationSave(params: VerificationSaveParams): Promise<VerificationSaveResult> {
    const saved = this.casSave(
      "verification.save",
      params,
      this.verificationCommands,
      cloneJson(params.command),
    );
    return { command: cloneJson(saved) };
  }

  async artifactBegin(params: ArtifactBeginParams): Promise<ArtifactBeginResult> {
    const replay = this.replayStored<ArtifactBeginResult>("artifact.begin", params.request_id, params);
    if (replay) return replay;
    if (params.mission_id !== null) this.requireMission(params.mission_id);
    const uploadId = this.opts.uuid();
    this.uploads.set(uploadId, {
      uploadId,
      missionId: params.mission_id,
      mediaType: params.media_type,
      expectedBytes: this.parseU64(params.bytes, "bytes"),
      expectedSha256: params.sha256.toLowerCase(),
      bytes: new Uint8Array(0),
      committed: null,
    });
    const result: ArtifactBeginResult = { upload_id: uploadId, chunk_bytes: ARTIFACT_CHUNK_BYTES };
    this.requests.set(params.request_id, {
      fingerprint: fingerprintOf("artifact.begin", params),
      result: cloneJson(result),
    });
    return result;
  }

  async artifactWrite(params: ArtifactWriteParams): Promise<ArtifactWriteResult> {
    const upload = this.requireUpload(params.upload_id);
    const offset = this.parseU64(params.offset, "offset");
    const bytes = base64ToBytes(params.data_b64);
    if (offset > upload.bytes.length) {
      // 건너뛴 offset — 이어붙임 지점만 허용(§3).
      throw new RpcClientError(
        "INVALID_ARGUMENT",
        `offset ${offset}은 현재 길이 ${upload.bytes.length}를 넘을 수 없습니다.`,
      );
    }
    if (offset < upload.bytes.length) {
      // 이전 offset 재전송: 같은 bytes면 같은 next_offset, 다르면 REQUEST_CONFLICT.
      const stored = upload.bytes.subarray(offset, offset + bytes.length);
      if (!bytesEqual(stored, bytes)) {
        throw new RpcClientError("REQUEST_CONFLICT", "같은 offset에 다른 내용이 재전송되었습니다.");
      }
      return { next_offset: String(offset + bytes.length) };
    }
    const merged = new Uint8Array(upload.bytes.length + bytes.length);
    merged.set(upload.bytes);
    merged.set(bytes, upload.bytes.length);
    upload.bytes = merged;
    return { next_offset: String(merged.length) };
  }

  async artifactCommit(params: ArtifactCommitParams): Promise<ArtifactRef> {
    const upload = this.requireUpload(params.upload_id);
    if (upload.committed) return { ...upload.committed };
    const sha256 = await sha256HexAuto(upload.bytes);
    if (upload.bytes.length !== upload.expectedBytes || sha256 !== upload.expectedSha256) {
      throw new RpcClientError(
        "INTEGRITY_FAILED",
        `업로드 무결성 검사 실패: bytes ${upload.bytes.length}/${upload.expectedBytes}, sha256 불일치`,
      );
    }
    const ref: ArtifactRef = {
      id: this.opts.uuid(),
      sha256,
      bytes: String(upload.bytes.length),
      media_type: upload.mediaType,
    };
    upload.committed = ref;
    this.artifacts.set(ref.id, { ref, bytes: upload.bytes });
    return { ...ref };
  }

  async artifactRead(params: ArtifactReadParams): Promise<ArtifactReadResult> {
    const artifact = this.artifacts.get(params.artifact_id);
    if (!artifact) throw new RpcClientError("NOT_FOUND", "알 수 없는 artifact입니다.");
    const offset = this.parseU64(params.offset, "offset");
    if (offset > artifact.bytes.length) {
      throw new RpcClientError("INVALID_ARGUMENT", "offset이 artifact 길이를 벗어났습니다.");
    }
    // 읽기 단위는 4 KiB 이하(§3).
    const take = Math.min(Math.max(params.max_bytes, 0), ARTIFACT_CHUNK_BYTES);
    const end = Math.min(offset + take, artifact.bytes.length);
    return {
      data_b64: bytesToBase64(artifact.bytes.subarray(offset, end)),
      next_offset: String(end),
      complete: end >= artifact.bytes.length,
    };
  }

  /** 시험 헬퍼: O1 fake store(mission·upload·설정·원장)를 통째로 비운다. */
  resetMissions(): void {
    this.missionStates.clear();
    this.snapshots.clear();
    this.uploads.clear();
    this.artifacts.clear();
    this.requests.clear();
    this.bindings.clear();
    this.templates.clear();
    this.verificationCommands.clear();
    this.repositoryIds.clear();
    this.dirtyEntryCounts.clear();
  }

  /** 현재 mission projection 목록(방어 복사, 생성 역순). */
  get missions(): Mission[] {
    return [...this.missionStates.values()]
      .map((state) => cloneJson(state.mission))
      .sort((a, b) => (a.created_at < b.created_at ? 1 : a.created_at > b.created_at ? -1 : 0));
  }

  /** 시험 헬퍼: 미션 entity를 직접 심는다(decision/finding/task 시나리오). */
  seedMissionEntity(missionId: string, entity: Entity): void {
    const state = this.requireMission(missionId);
    state.entities.set(entity.value.id, entity);
  }

  /**
   * 시험 헬퍼(O16): entity 여럿을 심고 실제 transaction처럼 commit한다 —
   * revision/event seq가 올라가고 mission.changed hint가 나가므로 UI
   * store의 dirty → 재동기화 경로를 진짜와 같게 걸을 수 있다. Mission
   * entity가 포함되면 그 값을 projection 정본에도 반영한다(phase/candidate
   * 교체 시나리오).
   */
  seedMissionEntities(missionId: string, entities: readonly Entity[]): void {
    const state = this.requireMission(missionId);
    const changes: Change[] = [];
    for (const entity of entities) {
      if (entity.kind === "mission" && entity.value.id === missionId) {
        state.mission = entity.value;
      }
    }
    for (const entity of entities) {
      const id = entity.value.id;
      state.entities.set(id, entity);
      changes.push({ entity_kind: entityKindOf(entity), entity_id: id, operation: "upsert" });
    }
    this.commitMissionTransaction(state, "changed", changes);
  }

  // ------------------------------------------------------ O1 mission internals

  /**
   * 공용 mutation 경로(§2): (1) request_id 재전송은 저장된 최초 응답 반환,
   * 다른 지문은 REQUEST_CONFLICT. (2) expected_revision 불일치는
   * REVISION_CONFLICT + details.current_revision. (3) 검증 통과 시 단일
   * transaction으로 commit하고 mission.changed hint를 쌓는다.
   */
  private missionMutation(
    method: string,
    params: MockMutationParams,
    type: MissionEventType,
    apply: (state: MockMissionState) => Change[],
  ): MutationResult {
    const replay = this.replayStored<MutationResult>(method, params.request_id, params);
    if (replay) return replay;
    const state = this.requireMission(params.mission_id);
    if (params.expected_revision !== state.mission.revision) {
      throw new RpcClientError("REVISION_CONFLICT", "기대한 revision이 현재 미션 revision과 다릅니다.", false, {
        current_revision: state.mission.revision,
      });
    }
    const changes = apply(state);
    const result = this.commitMissionTransaction(state, type, changes);
    this.requests.set(params.request_id, {
      fingerprint: fingerprintOf(method, params),
      result: cloneJson(result),
    });
    return result;
  }

  /**
   * mission transaction 1건 commit(§1·§2): revision·event seq를 함께 +1,
   * event 하나에 바뀐 entity ID를 모으고, projection의 Mission entity를
   * 갱신한 뒤 mission.changed hint를 pendingEvents에 쌓는다.
   */
  private commitMissionTransaction(
    state: MockMissionState,
    type: MissionEventType,
    changes: Change[],
  ): MutationResult {
    const revision = Number(state.mission.revision) + 1;
    const seq = state.events.length + 1;
    state.mission = { ...state.mission, revision: String(revision), updated_at: new Date().toISOString() };
    state.entities.set(state.mission.id, { kind: "mission", value: state.mission });
    state.events.push({
      mission_id: state.mission.id,
      seq: String(seq),
      revision: String(revision),
      transaction_id: this.opts.uuid(),
      type,
      changes: changes.map((c) => ({ ...c })),
      changes_ref: null,
      created_at: new Date().toISOString(),
    });
    this.emit({
      kind: "mission.changed",
      payload: { mission_id: state.mission.id, latest_seq: String(seq) },
    });
    this.scheduleFlush();
    return {
      mission_id: state.mission.id,
      revision: String(revision),
      event_seq: String(seq),
      entity_ids: changes.map((c) => c.entity_id),
    };
  }

  /**
   * request_id 지문 대조(§2): 같은 지문이면 저장된 최초 응답을 돌려주고,
   * 다르면 REQUEST_CONFLICT. 지문은 method + 정규화 payload(request_id
   * 제외, object key 재귀 정렬, array 순서 유지)의 조합이다.
   */
  private replayStored<T>(method: string, requestId: string, params: unknown): T | undefined {
    const entry = this.requests.get(requestId);
    if (!entry) return undefined;
    if (entry.fingerprint !== fingerprintOf(method, params)) {
      throw new RpcClientError("REQUEST_CONFLICT", "같은 request_id에 다른 payload가 재전송되었습니다.");
    }
    return cloneJson(entry.result) as T;
  }

  /**
   * binding/template/verification 공용 CAS 저장(§3): 신규는
   * expected_revision "0", 기존은 저장 revision과 비교. 반환 revision은
   * 서버가 +1해 1부터 시작한다.
   */
  private casSave<T extends { id: string; revision: string }>(
    method: string,
    params: { request_id: string; expected_revision: string },
    store: Map<string, T>,
    doc: T,
  ): T {
    const replay = this.replayStored<T>(method, params.request_id, params);
    if (replay) return replay;
    const existing = store.get(doc.id);
    const current = existing ? existing.revision : "0";
    if (params.expected_revision !== current) {
      throw new RpcClientError("REVISION_CONFLICT", "기대한 revision이 저장된 revision과 다릅니다.", false, {
        current_revision: current,
      });
    }
    const saved = { ...doc, revision: String(Number(current) + 1) };
    store.set(saved.id, saved);
    this.requests.set(params.request_id, {
      fingerprint: fingerprintOf(method, params),
      result: cloneJson(saved),
    });
    return cloneJson(saved);
  }

  private requireMission(id: string): MockMissionState {
    const state = this.missionStates.get(id);
    if (!state) throw new RpcClientError("NOT_FOUND", "알 수 없는 미션입니다.");
    return state;
  }

  private requireUpload(id: string): MockUpload {
    const upload = this.uploads.get(id);
    if (!upload) throw new RpcClientError("NOT_FOUND", "알 수 없는 업로드입니다.");
    return upload;
  }

  /** 10진 문자열 U64만 허용(§1) — 그 외는 INVALID_ARGUMENT. */
  private parseU64(value: string, field: string): number {
    if (!/^\d+$/.test(value)) {
      throw new RpcClientError("INVALID_ARGUMENT", `${field}는 10진 문자열이어야 합니다.`);
    }
    return Number(value);
  }

  private parseCursor(cursor: string): number {
    const value = this.parseU64(cursor, "cursor");
    if (value > Number.MAX_SAFE_INTEGER) {
      throw new RpcClientError("INVALID_ARGUMENT", "cursor가 너무 큽니다.");
    }
    return value;
  }

  /** 시험 헬퍼: 개입 알림을 쌓고 이벤트까지 흘린다(멱등 dedup 포함). */
  pushIntervention(notice: InterventionNotice): boolean {
    if (this.interventionRing.some((n) => n.report_id === notice.report_id)) return false;
    this.interventionRing.unshift(notice);
    this.interventionRing = this.interventionRing.slice(0, 100);
    this.emit({ kind: "intervention.reported", payload: notice });
    return true;
  }

  // ------------------------------------------------------- test-only helpers

  /** Append program output without going through input (slow-consumer test). */
  emitProgramOutput(sessionId: string, bytes: Uint8Array): void {
    const session = this.requireSession(sessionId);
    for (let off = 0; off < bytes.length; off += OUTPUT_CHUNK_BYTES) {
      this.appendRecord(session, { kind: "output", bytes: bytes.slice(off, off + OUTPUT_CHUNK_BYTES) });
    }
    this.scheduleFlush();
  }

  /** Terminate a session as if the process exited. */
  killSession(sessionId: string, exitCode = 0): void {
    const session = this.requireSession(sessionId);
    session.exited = true;
    const w = this.workloads.get(session.workloadId);
    this.emit({ kind: "session.exited", payload: { session_id: sessionId, exit_code: exitCode, descendants_remaining: false, reason: "process_exit" } });
    if (w && this.isLive(w.state)) {
      w.state = "DRAINING";
      this.revision++;
      this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
      this.schedule(() => {
        w.exitCode = exitCode;
        w.state = exitCode === 0 ? "SUCCEEDED" : "FAILED";
        this.emit({ kind: "workload.changed", payload: this.workloadSummary(w) });
        this.scheduleFlush();
      });
    }
    this.scheduleFlush();
  }

  /** Force a queue re-evaluation (wait reasons refresh). */
  refreshQueue(waitReason: QueueReason | null): void {
    for (const id of this.queueOrder) {
      const w = this.workloads.get(id);
      if (w && w.state === "QUEUED") w.queueReason = waitReason;
    }
    this.revision++;
    this.emit({ kind: "queue.changed", payload: { queue: this.queueEntries(), revision: this.revision } });
    this.scheduleFlush();
  }

  inspect(sessionId: string): {
    epoch: string;
    recordCount: number;
    ackedSeq: number;
    lastDeliveredSeq: number;
    unackedBytes: number;
    blocked: boolean;
    views: string[];
  } {
    const s = this.requireSession(sessionId);
    return {
      epoch: s.epoch,
      recordCount: s.records.length,
      ackedSeq: s.ackedSeq,
      lastDeliveredSeq: s.lastDeliveredSeq,
      unackedBytes: s.unackedBytes,
      blocked: s.blocked,
      views: [...s.views.keys()],
    };
  }

  workloadSummaries(): WorkloadSummary[] {
    return [...this.workloads.values()].map((w) => this.workloadSummary(w));
  }

  emitResourceSnapshot(): void {
    this.hostSample = this.nextHostSample();
    this.emit({ kind: "resource.snapshot", payload: this.hostSample });
    this.scheduleFlush();
  }

  // ---------------------------------------------------------------- internal

  private writerViewOf(session: MockSession): string | null {
    // sessionInput arrives on the data connection bound to the writer view.
    return session.writerViewId;
  }

  private appendRecord(session: MockSession, rec: Omit<MockRecord, "seq">): number {
    const seq = session.records.length + 1;
    session.records.push({ ...rec, seq });
    return seq;
  }

  /** Deliver journal records to attached views under flow control. */
  private pumpSession(session: MockSession): boolean {
    if (session.views.size === 0) return false;
    let delivered = false;
    while (
      !session.blocked &&
      session.lastDeliveredSeq < session.records.length
    ) {
      const rec = session.records[session.lastDeliveredSeq];
      if (!rec) break;
      const payload: SessionOutput = {
        session_id: session.id,
        epoch: session.epoch,
        seq: String(rec.seq),
        kind: rec.kind,
        data_b64: rec.bytes ? bytesToBase64(rec.bytes) : "",
        raw_len: rec.bytes?.length ?? 0,
        ...(rec.kind === "resize" ? { cols: rec.cols ?? 0, rows: rec.rows ?? 0 } : {}),
      };
      this.emit({ kind: "session.output", payload });
      session.lastDeliveredSeq = rec.seq;
      session.unackedBytes += payload.raw_len;
      delivered = true;
      if (session.unackedBytes >= this.opts.outputHighBytes) {
        session.blocked = true;
        this.emit({
          kind: "session.flow_blocked",
          payload: {
            session_id: session.id,
            blocked: true,
            view_id: session.writerViewId,
            unacked_bytes: String(session.unackedBytes),
          },
        });
      }
    }
    return delivered;
  }

  private emit(event: DaemonEvent): void {
    this.pendingEvents.push(event);
  }

  private schedule(fn: () => void): void {
    this.opts.schedule(() => {
      if (this.disposed) return;
      fn();
      this.flush();
    });
  }

  private scheduleFlush(): void {
    if (this.flushScheduled) return;
    this.flushScheduled = true;
    this.opts.schedule(() => {
      this.flushScheduled = false;
      this.flush();
    });
  }

  private flush(): void {
    if (this.disposed) return;
    let guard = 0;
    do {
      // Pump first so newly appended records join this flush.
      let pumped = false;
      for (const session of this.sessions.values()) {
        if (this.pumpSession(session)) pumped = true;
      }
      const events = this.pendingEvents;
      this.pendingEvents = [];
      for (const ev of events) {
        for (const listener of this.listeners) listener(ev);
      }
      if (!pumped && this.pendingEvents.length === 0) break;
      if (++guard > 100) break; // safety valve
    } while (true);
  }

  private requireSession(id: string): MockSession {
    const s = this.sessions.get(id);
    if (!s) throw new RpcClientError("INVALID_ARGUMENT", "알 수 없는 세션입니다.");
    return s;
  }

  private requireWorkload(id: string): MockWorkload {
    const w = this.workloads.get(id);
    if (!w) throw new RpcClientError("INVALID_ARGUMENT", "알 수 없는 작업입니다.");
    return w;
  }

  private isLive(state: WorkloadState): boolean {
    return state === "RUNNING" || state === "STARTING" || state === "STOPPING";
  }

  private workloadSummary(w: MockWorkload): WorkloadSummary {
    const session = w.sessionId ? this.sessions.get(w.sessionId) : undefined;
    const programName = w.request.program.replace(/^.*[/\\]/, "");
    return {
      workload_id: w.id,
      session_id: w.sessionId,
      mode: w.mode,
      state: w.state,
      priority: w.request.priority,
      title: w.mode === "shell" ? `${programName} (shell)` : [programName, ...w.request.argv].join(" "),
      cwd: w.request.cwd,
      program: w.request.program,
      reservation_bytes: w.request.policy.reservation_bytes,
      cpu_slots: w.request.policy.cpu_slots,
      enforcement: w.request.policy.enforcement,
      root_exited: w.rootExited,
      cancel_requested: w.cancelRequested,
      exit_code: w.exitCode,
      last_error_code: w.lastErrorCode,
      queue_reason: w.state === "QUEUED" ? ((w.queueReason ?? "WAIT_TELEMETRY") as WorkloadSummary["queue_reason"]) : null,
      connection: session && session.views.size > 0 ? "attached" : "detached",
      usage: this.isLive(w.state) ? mockUsage(w.id, this.revision) : null,
      relief: { ...w.relief },
      guard: { ...w.guard },
      guard_warning: null,
      protected: w.protected,
    };
  }

  private startResourceStream(): void {
    if (this.resourceTimer || this.opts.resourceIntervalMs <= 0 || this.disposed) return;
    this.resourceTimer = setInterval(() => this.emitResourceSnapshot(), this.opts.resourceIntervalMs);
  }

  private stopResourceStream(): void {
    if (this.resourceTimer) clearInterval(this.resourceTimer);
    this.resourceTimer = null;
  }

  private nextHostSample(): HostSample {
    this.resourceTick += 1;
    const t = this.opts.monotonicNow();
    const wave = Math.sin(this.resourceTick / 6) * 0.6;
    const GiB = 1024 ** 3;
    const total = 32 * GiB;
    const availableFrac = 0.34 + Math.sin(this.resourceTick / 9) * 0.06;
    const available = Math.round(total * availableFrac);
    return {
      monotonic_ms: t,
      physical_total_bytes: { value: String(total), source: "mock.host", quality: "measured", reason: null },
      physical_available_bytes: { value: String(available), source: "mock.host", quality: "measured", reason: null },
      swap_used_bytes: { value: String(Math.round(1.2 * GiB)), source: "mock.host", quality: "measured", reason: null },
      pressure: available < GiB ? "CRITICAL" : available / total < 0.12 ? "WARNING" : "NORMAL",
      cpu_pressure: "NORMAL",
      // 첫 differential sample은 null(03 §2).
      cpu_cores_used: {
        value: this.resourceTick === 1 ? null : Number((2.4 + wave).toFixed(2)),
        source: "mock.cpu",
        quality: "measured",
        reason: null,
      },
      logical_cpu_count: 12,
      disks: [
        {
          mount: "C:\\",
          capacity_bytes: { value: String(512 * GiB), source: "mock.fs", quality: "measured", reason: null },
          free_bytes: {
            value: String(Math.round((180 + Math.sin(this.resourceTick / 15) * 2) * GiB)),
            source: "mock.fs",
            quality: "measured",
            reason: null,
          },
        },
      ],
      interfaces: [
        {
          name: "ethernet",
          rx_bytes_per_sec: {
            value: this.resourceTick === 1 ? null : Number((1.2 + Math.abs(wave) / 2).toFixed(2)) * 1024 * 1024,
            source: "mock.net",
            quality: "measured",
            reason: null,
          },
          tx_bytes_per_sec: {
            value: this.resourceTick === 1 ? null : Number((0.2 + Math.abs(wave) / 3).toFixed(2)) * 1024 * 1024,
            source: "mock.net",
            quality: "measured",
            reason: null,
          },
          is_loopback: false,
        },
        {
          name: "loopback",
          rx_bytes_per_sec: { value: 2048, source: "mock.net", quality: "measured", reason: null },
          tx_bytes_per_sec: { value: 2048, source: "mock.net", quality: "measured", reason: null },
          is_loopback: true,
        },
      ],
    };
  }
}

/**
 * 데몬의 `ORDER BY last_seen_at DESC, id DESC`와 같은 정렬(ISO-8601 UTC는
 * 문자열 비교 순서가 시각 순서와 같다). 0보다 작으면 a가 더 최근이다.
 */
function compareAgentSessions(a: AgentSessionRecord, b: AgentSessionRecord): number {
  if (a.last_seen_at !== b.last_seen_at) return a.last_seen_at < b.last_seen_at ? 1 : -1;
  if (a.id === b.id) return 0;
  return a.id < b.id ? 1 : -1;
}

/** Wire DTO는 JSON 안전 — 호출부가 fake store를 못 건드리도록 깊은 복사. */
function cloneJson<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

/** request_id 멱등 지문(§2): method + 정규화 payload. */
function fingerprintOf(method: string, params: unknown): string {
  return `${method}:${canonicalJson(params)}`;
}

/** object key 재귀 정렬, array 순서 유지, null 포함, request_id만 제외(§2). */
function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const body = Object.entries(value as Record<string, unknown>)
      .filter(([key]) => key !== "request_id")
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
      .map(([key, val]) => `${JSON.stringify(key)}:${canonicalJson(val)}`)
      .join(",");
    return `{${body}}`;
  }
  return JSON.stringify(value) ?? "null";
}

function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

function isMutationResult(value: unknown): value is MutationResult {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  return (
    typeof v.mission_id === "string" && typeof v.revision === "string" && typeof v.event_seq === "string"
  );
}

/** runtime.detect fixture가 보고하는 설치 버전(없으면 null). */
function mockInstalledVersion(runtime: Binding["runtime"]): string | null {
  switch (runtime) {
    case "codex":
      return MOCK_CODEX_VERSION;
    case "claude":
      return MOCK_CLAUDE_VERSION;
    default:
      return null;
  }
}

/** 런타임이 실제로 광고한다고 흉내내는 모델 목록(runtime.detect·bindingProbe가 공유). */
function mockAdvertisedModels(runtime: Binding["runtime"], providerId?: string): ProbeModel[] {
  const ids = runtime === "codex" ? MOCK_CODEX_MODELS : runtime === "claude"
    ? (providerId === "zai-coding-plan" ? MOCK_CLAUDE_ZAI_MODELS : MOCK_CLAUDE_MODELS)
    : runtime === "opencode" ? MOCK_OPENCODE_MODELS : [];
  return ids.map((id) => ({ id, efforts: [] }));
}

/** 데몬 capability_evidence::adapter_supports_route 사본. */
function mockAdapterSupportsRoute(binding: Binding): boolean {
  const refs = binding.credential_ref !== null && binding.endpoint_ref !== null;
  const noRefs = binding.credential_ref === null && binding.endpoint_ref === null;
  switch (binding.runtime) {
    case "codex":
      return binding.provider_id === "openai"
        && ((binding.auth_route === "subscription" && noRefs) || (binding.auth_route === "api_key" && refs));
    case "claude":
      return (binding.provider_id === "anthropic" && binding.auth_route === "subscription" && noRefs)
        // Z.ai Coding Plan also launches on the daemon's own key store (no stored connection).
        || (binding.provider_id === "zai-coding-plan" && binding.auth_route === "subscription" && noRefs)
        || (["anthropic", "zai-coding-plan"].includes(binding.provider_id)
          && (binding.auth_route === "api_key" || binding.auth_route === "subscription") && refs);
    case "opencode":
      return refs && (binding.auth_route === "api_key" || binding.auth_route === "subscription") && binding.provider_id.trim() !== "";
    default:
      return false;
  }
}

/** 어댑터가 구현한 기능 목록(Fake는 없다) — 자가 진단과 동의가 열 수 있는 상한이다. */
function mockImplementedCapabilities(runtime: Binding["runtime"]): ReadonlyArray<keyof Binding["capabilities"]> {
  return runtime === "fake" ? [] : MOCK_ADAPTER_IMPLEMENTED[runtime];
}

/**
 * 무추론 자가 진단 흉내(11 §6): 프로토콜은 통과하고, Codex만 OS 경계 12사례를 돌며,
 * 모델 목록을 가진 런타임은 이 연결의 모델이 그 목록에 있는지 본다. 실패 slug는 없다.
 */
function mockLocalProbe(binding: Binding): LocalProbeReport {
  const codex = binding.runtime === "codex";
  const listing = mockAdvertisedModels(binding.runtime, binding.provider_id);
  return {
    protocol_ok: true,
    sandbox_cases_passed: codex ? MOCK_CODEX_SANDBOX_CASES : null,
    sandbox_cases_total: codex ? MOCK_CODEX_SANDBOX_CASES : null,
    model_listed: listing.length > 0 ? listing.some((model) => model.id === binding.model_id) : null,
    failures: [],
  };
}

const EMPTY_RUNS: LocalEvidence["runs"] = {
  succeeded_read_only: 0, succeeded_write: 0, cancelled: 0, invalid_result: 0, last_at: null,
};

/**
 * binding.probe가 저장하는 로컬 증거(11 §7): 같은 OS·버전·모델이면 실행 관측을 보존하고
 * 자가 진단만 갈아 끼우며, 모델이 다르거나 버전이 바뀌면 관측을 0부터 다시 센다.
 */
function mockProbeEvidence(binding: Binding, version: string, probe: LocalProbeReport, now: string): LocalEvidence {
  const previous = binding.local_evidence ?? null;
  const same = previous !== null && previous.os === MOCK_OS && previous.version === version && previous.model_id === binding.model_id;
  return {
    os: MOCK_OS, version, model_id: binding.model_id, probed_at: now, probe,
    runs: same ? previous.runs : { ...EMPTY_RUNS },
  };
}

/**
 * binding.save는 클라이언트가 보낸 로컬 증거를 버리고 저장된 것을 이어 간다(11 §7):
 * 실행 대상이 같을 때만 이어 가고, 모델만 다르면 자가 진단은 남기되 실행 관측은 0으로 되돌린다.
 */
function mockCarriedEvidence(previous: Binding | undefined, next: Binding): LocalEvidence | null {
  const evidence = previous?.local_evidence ?? null;
  if (!previous || !evidence) return null;
  const sameTarget = previous.runtime === next.runtime && previous.program === next.program
    && previous.provider_id === next.provider_id && previous.auth_route === next.auth_route
    && previous.credential_ref === next.credential_ref && previous.endpoint_ref === next.endpoint_ref;
  if (!sameTarget) return null;
  if (evidence.model_id === next.model_id) return evidence;
  return { ...evidence, model_id: next.model_id, runs: { ...EMPTY_RUNS } };
}

/** archive 가능한 종료 상태(§6). */
function isTerminalMissionState(state: Mission["state"]): boolean {
  return state === "completed" || state === "failed" || state === "cancelled";
}

/** Entity union → EntityKind(시험 helper용). */
function entityKindOf(entity: Entity): Change["entity_kind"] {
  if (entity.kind === "mission") return "mission";
  if (entity.kind === "task") return "task";
  if (entity.kind === "run") return "run";
  if (entity.kind === "message") return "message";
  if (entity.kind === "decision") return "decision";
  if (entity.kind === "workspace") return "workspace";
  if (entity.kind === "candidate") return "candidate";
  if (entity.kind === "verification") return "verification";
  if (entity.kind === "finding") return "finding";
  return "knowledge";
}

function mockCapabilities(): Capabilities {
  return {
    memory_limit_kind: { support: "unsupported", reason: "mock executor는 OS 메모리 상한을 적용하지 않습니다" },
    cpu_quota: { support: "supported" },
    process_count_limit: { support: "supported" },
    tree_accounting: { support: "unsupported", reason: "mock" },
    reattach: { support: "supported" },
    resume: { support: "unsupported", reason: "R1: daemon 재시작 후 세션 resume 없음" },
    scheduling_yield: { support: "supported" },
    suspend_resume: { support: "supported" },
    platform: "mock",
    // 이 mock은 O1 mission RPC를 구현한다(O04) — 계약상 광고도 함께
    // 켠다. 없으면 UI가 mission 생성을 잠근다(O01 gate).
    mission_protocol: 1,
    // LaunchRequest.claude_provider(Claude Code의 Z.ai 라우팅)를 받아들인다는
    // 광고 — 키 해석은 실제 데몬만 하지만, 없으면 UI가 라우팅 실행을 거절한다.
    claude_provider_routing: true,
    notes: ["MockDaemonClient — dev/test 전용"],
  };
}

function hashString(text: string): number {
  let h = 2166136261;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return Math.abs(h);
}

/** include_uncommitted의 base_oid 흉내 — seed로부터 40자 16진 문자열(가짜 git oid)을 만든다. */
function fakeSnapshotOid(seed: string): string {
  const a = hashString(seed).toString(16).padStart(8, "0");
  const b = hashString(`${seed}:snapshot`).toString(16).padStart(8, "0");
  return `${a}${b}${a}${b}${a}`.slice(0, 40);
}

function mockUsage(workloadId: string, revision: number): WorkloadSummary["usage"] {
  const h = hashString(workloadId);
  const MiB = 1024 ** 2;
  const wobble = ((revision + (h % 7)) % 10) / 10;
  return {
    workload_id: workloadId,
    cpu_cores: { value: Number((0.3 + (h % 30) / 10 + wobble).toFixed(2)), source: "mock.proc", quality: "measured", reason: null },
    resident_bytes: { value: String(Math.round((120 + (h % 600) + wobble * 80) * MiB)), source: "mock.proc", quality: "measured", reason: null },
    accounted_bytes: { value: null, source: "cgroup.v2", quality: "unavailable", reason: "mock 플랫폼에는 cgroup 계상값이 없습니다" },
    committed_bytes: { value: null, source: "win32.job", quality: "unavailable", reason: "mock 플랫폼에는 job commit 값이 없습니다" },
    read_bytes_per_sec: { value: Number((h % 40) / 10) * MiB, source: "mock.io", quality: "estimated", reason: null },
    write_bytes_per_sec: { value: Number((h % 25) / 10) * MiB, source: "mock.io", quality: "estimated", reason: null },
    network_rx_bytes_per_sec: { value: 0, source: "mock.net", quality: "unavailable", reason: "프로세스별 네트워크는 공통 지표로 제공하지 않습니다" },
    network_tx_bytes_per_sec: { value: 0, source: "mock.net", quality: "unavailable", reason: "프로세스별 네트워크는 공통 지표로 제공하지 않습니다" },
    process_count: { value: 1 + (h % 8), source: "mock.proc", quality: "measured", reason: null },
    coverage: "observed_tree",
  };
}
