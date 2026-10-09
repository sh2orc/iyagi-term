/**
 * SessionController: the orchestration that React effects must NOT own
 * (04-ui.md §4, U09). Session launches, attaches, and closes are triggered
 * by user actions routed through this controller; component effects only
 * mount/unmount registry-owned DOM.
 */

import { usePreferences } from "../../store/preferences";
import { rememberedWorkload } from "../../store/workloadMemory";
import { isFinishedWorkload } from "../../store/workloadState";
import { cleanShellCommand, terminalDisplayTitle } from "./shellEnvironment";
import { clearTerminalActivity, markTerminalActivity, recordSessionOutput } from "./activity";
import { classifyInputRejection, inputIssueOf, setInputIssue } from "./inputHealth";
import { screenSignature } from "./screenOutput";
import { defaultShellArgv } from "./shellProfiles";
import type { LaunchRequest } from "../../generated/LaunchRequest";
import type { ReliefAction } from "../../generated/ReliefAction";
import type { ReliefState } from "../../generated/ReliefState";
import type { WorkloadSummary } from "../../generated/WorkloadSummary";
import type { DaemonClient, DaemonEvent } from "../daemon/client";
import { RpcClientError } from "../daemon/client";
import { t } from "../../i18n";
import type { ShortcutAction, Platform } from "./shortcuts";
import { SessionPipeline, type PipelineSnapshot } from "./pipeline";
import { REPLAY_SNAPSHOT_MAX_CHARS, type ReplaySnapshot, type ReplaySnapshotStore } from "./replaySnapshot";
import { isHistoryRebuilder, markHistoryRebuilder } from "./zoomPreview";
import type { FitDimensions, TerminalRegistry } from "./registry";
import { registerTerminalReporting } from "./xtermSetup";
import { paneBackgroundColor } from "./paneBackground";
import { terminalTheme } from "./terminalPalette";
import { resolveTheme, systemPrefersLight } from "../../app/theme";
import {
  detectInterventionPattern,
  PatternCooldown,
} from "./interventionPatterns";
import type { SessionSearchResult } from "../../generated/SessionSearchResult";
import {
  MAX_PANES_PER_TAB,
  canAddPane,
  canSplit,
  findLeaf,
  gridLayout,
  leafCount,
  listLeaves,
  makeLeaf,
  type PaneEdge,
  type SplitNode,
} from "./splitTree";
import { paneProjectKey, planRegroup, projectTitle, type RegroupPlan } from "./regroup";
import type { LayoutDropAction, LayoutPlacement } from "./layoutDrag";
import { isTerminalReport } from "./broadcast";
import { paneLimitText, splitNoSpaceText } from "../monitor/statusStrings";
import { inspectPaste, preparePaste, bracketedPasteEnabled, unwrapBracketedPaste } from "../security/paste";
import { acceptReportedCwd } from "./osc";
import { ancestorDirs, filesystemRoot } from "./cwdFallback";
import { isCleanExit, pasteTooLargeText, type PanePhase } from "../monitor/statusStrings";
import {
  PANE_RELIEF_NONE,
  clearOwnedToast,
  sameRelief,
  showOwnedToast,
  useWorkbenchStore,
  type PaneMeta,
  type PaneRelief,
} from "../../store/workbenchStore";
import {
  interventionKindKey,
  maybeDesktopNotify,
  useNotificationStore,
} from "../notifications/notificationStore";
import { workloadStateText } from "../monitor/statusStrings";
import { resourceHistory } from "../monitor/graphRing";
import { parseU64 } from "../monitor/format";
import { clampFontSize, zoomFontSize, type ZoomStep } from "./zoom";
import {
  collectMemoryDiagnostics,
  formatMemoryDiagnostics,
} from "./memoryDiagnostics";
import type { QuitPort } from "../app/quit";
import type { AgentSessionRecord } from "../../generated/AgentSessionRecord";
import type { AgentStatus } from "../../generated/AgentStatus";
import {
  decodeResumeInfo,
  resumeArgv,
  resumeInfoFrom,
  type AgentResumeInfo,
  type ResumableAgent,
} from "../agentSessions/types";
import { autonomyEnabled } from "../workloads/autonomy";
import type { ClaudeProvider } from "../../generated/ClaudeProvider";
import type { CliKind } from "../profiles/types";
import {
  CLAUDE_PROVIDER_DAEMON_OUTDATED_KEY,
  claudeProviderErrorMessage,
  claudeProviderFor,
} from "../workloads/claudeProvider";
import { hoveredLink } from "./linkHover";
import { dropPayload, quoteDropPath } from "./dropPaste";
import { PASTE_IMAGE_MAX_BYTES, pickImageType, type PasteImageSource } from "./clipboardImage";
import type { PasteImagePort } from "./pasteImage";
import { clearTerminalHistory } from "./terminalClear";
import { loadClearMark, saveClearMark } from "./clearMarks";
import { agentDisplayName } from "./agentNames";
import { requestNewMission } from "../missions/entryPoints";

export interface ControllerConfig {
  projectRoot: string | null;
  home: string;
}

/**
 * 다음 pane이 열 셸. `agent`는 특정 CLI를 여는 셸(에이전트 재개)에만 붙는
 * 표식이다 — launchShell이 Claude 제공자 라우팅(Z.ai)을 붙일지 이걸로 정한다.
 * 일반 셸·셸 프로필에는 없고, 그때는 라우팅하지 않는다.
 */
export interface ShellSpec {
  program: string;
  argv: string[];
  label: string;
  agent?: CliKind;
}

export interface ControllerDeps {
  client: DaemonClient;
  registry: TerminalRegistry;
  platform: Platform;
  /** 새 터미널에 쓸 셸(프로필 선택 UI가 주입). 없으면 플랫폼 기본값. */
  resolveShell?: () => { program: string; argv: string[]; label: string };
  /**
   * 에이전트 세션을 이어서 열 때 쓸 실행 파일(04-ui §5). 기록에 절대
   * 경로가 없을 때만 부르며, 시스템 probe(listClis)가 찾은 후보를 준다.
   * 못 찾으면 null — 컨트롤러는 추측해서 실행하지 않는다.
   */
  resolveAgentProgram?: (kind: ResumableAgent) => Promise<string | null>;
  config?: Partial<ControllerConfig>;
  uuid?: () => string;
  /** scrollback 검색 위임(실제 구현은 xtermSetup의 SearchAddon). */
  searchHandler?: (terminal: unknown, query: string, direction: "next" | "previous") => boolean;
  /** 앱 종료 포트(Tauri). 브라우저 미리보기·시험은 생략 가능. */
  quit?: QuitPort;
  /**
   * 클립보드·드롭 그림을 임시 파일로 떨구는 포트(Tauri). 없으면 그림
   * 붙여넣기는 건너뛰고 글 경로만 남는다 — 브라우저 미리보기·시험.
   */
  savePasteImage?: PasteImagePort;
  /**
   * 화면 스냅샷 저장소(IndexedDB, replaySnapshot.ts). `serializeTerminal`과 함께
   * 있어야 쓴다 — 없으면 attach는 늘 저널 전체를 재생한다.
   */
  replaySnapshots?: ReplaySnapshotStore;
  /** xterm 화면 직렬화(xtermSetup.serializeTerminalState). */
  serializeTerminal?: (terminal: unknown) => { data: string; cols: number; rows: number } | null;
}

const SESSIONS_LIMIT = 32; // defaults.json limits.sessions
const TRANSPORT_CHECK_MS = 2000;
/**
 * 전송 복구(모든 pane 재접속)의 backoff. 복구가 잇달아 필요하면(브리지가
 * 계속 흔들리거나 데몬이 재접속마다 다시 끊으면) 간격을 2배씩 늘린다 —
 * 2초마다 모든 터미널이 reset+재생으로 깜빡이는 폭주를 막는다. 마지막
 * 복구 뒤 TRANSPORT_HEALTHY_MS 동안 조용하면 처음부터 다시 센다.
 */
const TRANSPORT_RECOVER_BACKOFF_BASE_MS = 2000;
const TRANSPORT_RECOVER_BACKOFF_MAX_MS = 60000;
const TRANSPORT_HEALTHY_MS = 30000;
/**
 * 데몬이 view를 떼고 다시 붙으라는 알림(`session.replay_required`)의 처리
 * backoff. 같은 pane에 알림이 잇달으면 한 번으로 합치고 간격을 2배씩 늘린다.
 * REATTACH_STORM_WINDOW_MS 안에 REATTACH_STORM_LIMIT를 넘기면 무한 reset+
 * 재생 대신 실패 오버레이(재시도 있음)로 끝낸다. live로 REPLAY_HEALTHY_MS를
 * 버티면 횟수를 지운다.
 */
const REATTACH_BACKOFF_BASE_MS = 1000;
const REATTACH_BACKOFF_MAX_MS = 15000;
const REATTACH_STORM_WINDOW_MS = 60000;
const REATTACH_STORM_LIMIT = 6;
const REPLAY_HEALTHY_MS = 10000;
/** 주기 스냅샷 점검 간격 — 한 번에 가장 오래된 세션 하나만 뜬다(직렬화는 메인 스레드). */
const SNAPSHOT_TICK_MS = 5000;
/** 같은 세션을 주기적으로 다시 뜨는 최소 간격(출력이 계속 바뀌는 세션). */
const SNAPSHOT_MIN_INTERVAL_MS = 60000;
/** 탭을 숨긴 뒤 뜨기까지 기다리는 시간 — 탭 전환 순간에 직렬화 비용을 싣지 않는다. */
const SNAPSHOT_HIDDEN_DELAY_MS = 1000;
/** 이만큼 재생했으면 재생이 끝나자마자 스냅샷을 떠 다음 시작을 빠르게 한다. */
const SNAPSHOT_AFTER_REPLAY_BYTES = 256 * 1024;
/** 종료 직전 스냅샷 저장을 기다리는 상한. */
const SNAPSHOT_QUIT_FLUSH_MS = 1500;
/** 앱 시작 때 활성 탭 재생을 먼저 끝내도록 나머지 pane 연결을 미루는 상한. */
const DEFERRED_ATTACH_MAX_WAIT_MS = 3000;
/** 전체 강제 종료와 종료 상태 확인의 대기 상한. 실패하면 앱을 유지한다. */
const QUIT_CANCEL_TIMEOUT_MS = 10000;
/** 새 pane의 PTY 크기를 재려고 mount를 기다리는 프레임 수 상한. */
const LAUNCH_MEASURE_FRAMES = 3;
/** live 출력 묶음 창(ms): 한 TUI 프레임의 조각들을 한 번의 write로 모은다(pipeline.ts). */
const LIVE_OUTPUT_COALESCE_MS = 16;
/**
 * 재생 바이트 예산(AttachParams.max_replay_bytes). 살아 있는 세션은 뒤쪽 몇 MiB면
 * 충분하다 — 재생이 끝나면 크기를 흔들어 TUI가 화면을 다시 그린다. 끝난 세션은
 * 다시 그려 줄 프로그램이 없으니 마지막 화면을 되살리도록 더 넉넉히 잡되, 세션
 * 저널 상한(128 MiB)을 통째로 xterm에 밀어 넣지는 않는다(앱 시작의 수 초 청킹).
 */
const REPLAY_BUDGET_LIVE_BYTES = 8 * 1024 * 1024;
const REPLAY_BUDGET_FINISHED_BYTES = 32 * 1024 * 1024;

export function defaultShellProgram(platform: Platform): string {
  switch (platform) {
    case "windows":
      return "C:\\Windows\\System32\\cmd.exe";
    case "darwin":
      return "/bin/zsh";
    default:
      return "/bin/bash";
  }
}

/**
 * 감지된 에이전트를 값으로 비교한다(04-ui §5-1).
 *
 * 텔레메트리는 내용이 같아도 매 틱 새 객체를 실어 보내므로 참조로 비교하면
 * 모든 pane의 header가 초마다 다시 그려진다. 데몬 쪽 판정(agent_watch
 * `agent_status_eq`)과 같은 뜻이 되도록 키 합집합을 얕게 비교하고,
 * 없는 키·undefined·null은 같은 것으로 본다 — 계약에 선택 필드가 늘어도
 * 비교가 저절로 따라간다.
 */
function sameAgentStatus(a: AgentStatus | null, b: AgentStatus | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  const keys = new Set<string>([...Object.keys(a), ...Object.keys(b)]);
  for (const key of keys) {
    const left = (a as Record<string, unknown>)[key] ?? null;
    const right = (b as Record<string, unknown>)[key] ?? null;
    if (left !== right) return false;
  }
  return true;
}

function defaultShellPolicy(): LaunchRequest["policy"] {
  return {
    reservation_bytes: "2147483648",
    cpu_slots: 1,
    enforcement: "observe",
    memory_max_bytes: null,
    cpu_max_cores: null,
    pids_max: null,
  };
}

/** 받아들여진 시작 요청: 데몬 응답, 실제로 연 경로, 대체 경로로 간 첫 사유(없으면 null). */
interface LaunchAttempt {
  outcome: Awaited<ReturnType<DaemonClient["workloadLaunch"]>>;
  cwd: string;
  cwdReason: string | null;
}

/** 같은 에이전트 대화를 실행 중인 작업·pane(찾은 만큼). */
interface LiveConversation {
  workloadId: string | null;
  sessionId: string | null;
  leafId: string | null;
}

/** 끝난 작업을 되살리는 방법(최근 종료의 터미널 연결). */
type RecoveryTarget =
  | { kind: "resume"; info: AgentResumeInfo }
  /** 같은 대화가 이미 다른 작업에서 실행 중이다 — 새로 열지 않고 그리로 간다. */
  | { kind: "running"; live: LiveConversation | null }
  /** 에이전트 기록이 없는 일반 터미널 — 이전 출력 아래에서 새 셸로 잇는다. */
  | { kind: "shell" };

/** 새 셸이 이을 끝난 작업(재생이 끝나기를 기다리는 복구). */
interface PendingShellRecovery {
  /** 재생 중인 이전 세션 — 이 세션의 재생이 끝나야 새 셸을 띄운다. */
  sessionId: string;
  fromWorkloadId: string;
  cwd: string;
}

/**
 * 복구한 새 셸을 이전 출력 아래에 이어 붙이기 전의 화면 정리와 구분선. 끝난
 * 프로그램이 켜 둔 대체 화면·마우스 보고를 끄고 DECSTR로 나머지 모드·글자 속성을
 * 되돌린다(대체 화면 해제는 커서를 옮기지 않는 47번을 쓴다). xterm에만 쓰고 PTY·
 * 저널에는 보내지 않는다.
 */
function recoverySeparator(): string {
  const reset = "\x1b[?47l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[!p";
  return `${reset}\r\n\x1b[2m── ${t("terminal.recover.separator")} ──\x1b[0m\r\n`;
}

export class SessionController {
  private readonly pipelines = new Map<string, SessionPipeline>(); // viewId → pipeline
  private readonly sessionIndex = new Map<string, { viewId: string; leafId: string }>();
  private readonly workloadSession = new Map<string, string>(); // workloadId → sessionId
  private readonly paneSizes = new Map<string, { width: number; height: number }>();
  private readonly uuid: () => string;
  private readonly config: ControllerConfig;
  private restorePromise: Promise<void> | null = null;
  private eventSub: { dispose(): void } | null = null;
  /** focus 보고 구독 해제(zustand subscribe가 돌려주는 함수). */
  private focusSub: (() => void) | null = null;
  /**
   * 마지막으로 데몬에 알린 focus 세션. 새 제어 연결의 기본값이 "보는 세션
   * 없음"이므로 null로 시작하고, 재접속 때 다시 null로 되돌린다.
   */
  private reportedFocusSessionId: string | null = null;
  private toastTimer: ReturnType<typeof setTimeout> | null = null;
  private transportTimer: ReturnType<typeof setInterval> | null = null;
  private deferredDisposeTimer: ReturnType<typeof setTimeout> | null = null;
  private transportCheckInFlight = false;
  private transportGeneration: number | null = null;
  private transportRecovery: Promise<void> | null = null;
  private disposed = false;
  private liveSessions = new Set<string>();
  /** 데몬 재시작으로 끝난 세션 — 저널을 다시 재생해 종료로 돌아가도 그 사유를 남긴다. */
  private readonly daemonRestartedSessions = new Set<string>();
  private detachedPendingLaunches = new Set<string>();
  /** 세션별 마지막으로 저장(또는 복원)한 스냅샷 seq·시각 — 바뀐 화면만 다시 뜬다. */
  private readonly snapshotMarks = new Map<string, { seq: number; at: number }>();
  /** 진행 중인 스냅샷 저장(종료 직전 기다린다). */
  private readonly snapshotSaves = new Set<Promise<void>>();
  /** 재생 막힘으로 재접속한 횟수(leaf id별) — 무한 재시도를 막는다. */
  private readonly replayStallStrikes = new Map<string, number>();
  /** 데몬 강제 재접속(`session.replay_required`)의 leaf별 backoff 상태. */
  private readonly reattachBackoff = new Map<
    string,
    { count: number; windowStart: number; timer: ReturnType<typeof setTimeout> | null }
  >();
  /** live로 버틴 시간을 재는 타이머(leaf id별) — 만료되면 strikes·backoff를 지운다. */
  private readonly healthyTimers = new Map<string, ReturnType<typeof setTimeout>>();
  /** 마지막 전송 복구 시작 시각과 연속 횟수(backoff 계산). */
  private transportRecoverAt = 0;
  private transportRecoverStreak = 0;
  private snapshotTimer: ReturnType<typeof setInterval> | null = null;
  private visibilitySub: (() => void) | null = null;
  /** 앱 시작 때 활성 탭 재생이 끝나기를 기다리며 미룬 pane 연결(leaf id별). */
  private readonly deferredAttaches = new Map<string, { leafId: string; sessionId: string; workloadId: string }>();
  /** 아직 재생이 끝나지 않은 활성 탭 pane(leaf id). 비면 미룬 연결을 시작한다. */
  private readonly foregroundReplays = new Set<string>();
  private deferredAttachTimer: ReturnType<typeof setTimeout> | null = null;
  private deferredAttachRun: Promise<void> | null = null;

  private nextShell: ShellSpec | null = null;
  /** 종료 결정이 진행 중이면 그 약속(중복 클릭·중복 요청 합치기). */
  private quitInFlight: Promise<void> | null = null;
  /** 패턴 감지 쿨다운(W3-5): 재발화 억제. */
  private readonly patternCooldown = new PatternCooldown();
  /** 세션별 출력 꼬리(청크 경계를 가로지르는 문구 매칭용). */
  private readonly patternTails = new Map<string, string>();

  constructor(private readonly deps: ControllerDeps) {
    this.uuid = deps.uuid ?? (() => crypto.randomUUID());
    this.config = {
      projectRoot: deps.config?.projectRoot ?? null,
      home: deps.config?.home ?? (deps.platform === "windows" ? "C:\\Users" : "/home"),
    };
    // 표시/숨김 → 숨은 view의 pipeline 표시 빈도를 낮춘다(04-ui §4). registry가
    // mount/unmount에서 부른다. 아직 pipeline이 없는 view는 조용히 넘긴다.
    this.deps.registry.setVisibilityListener((viewId, visible) => {
      const pipeline = this.pipelines.get(viewId);
      pipeline?.setHidden(!visible);
      if (!visible && pipeline && this.deps.replaySnapshots && this.deps.serializeTerminal) {
        // 숨긴 탭의 화면을 저장해 둔다 — 전환 순간이 아니라 잠시 뒤에.
        setTimeout(() => void this.captureReplaySnapshot(viewId), SNAPSHOT_HIDDEN_DELAY_MS);
      }
      // 미뤄 둔 pane이 보이게 되면 기다리지 않고 바로 붙인다.
      if (visible && !pipeline) this.attachDeferredView(viewId);
    });
  }

  // ------------------------------------------------------------------ wiring

  /** Registry access for view components (mount/unmount only). */
  get registry(): TerminalRegistry {
    return this.deps.registry;
  }

  start(): void {
    if (this.deferredDisposeTimer) {
      clearTimeout(this.deferredDisposeTimer);
      this.deferredDisposeTimer = null;
    }
    if (this.disposed) return;
    if (this.eventSub) return;
    this.eventSub = this.deps.client.events.subscribe((event) => this.handleEvent(event));
    // 보고 있는 세션을 데몬에 알린다(08-pressure-relief §1). 스토어는
    // subscribeWithSelector를 쓰지 않으므로 파생값 비교는 여기서 한다.
    this.focusSub = useWorkbenchStore.subscribe(() => this.reportFocus());
    this.reportFocus();
    if (this.deps.client.transportStatus && this.deps.client.reconnectTransport) {
      this.transportTimer = setInterval(() => void this.checkTransport(), TRANSPORT_CHECK_MS);
    }
    if (this.deps.replaySnapshots && this.deps.serializeTerminal) {
      this.snapshotTimer = setInterval(() => this.snapshotTick(), SNAPSHOT_TICK_MS);
      if (typeof document !== "undefined" && typeof document.addEventListener === "function") {
        // 창이 숨겨지면(최소화·다른 앱) 바뀐 화면을 모두 뜬다 — 보는 사람이 없어 비용이 보이지 않는다.
        const onVisibility = () => {
          if (document.visibilityState === "hidden") void this.flushReplaySnapshots();
        };
        document.addEventListener("visibilitychange", onVisibility);
        this.visibilitySub = () => document.removeEventListener("visibilitychange", onVisibility);
      }
    }
    void this.restoreWorkspace();
  }

  /**
   * 시작 복원이 실패하면 실패를 cache에 남기지 않는다(다음 호출이 다시 시도하게)
   * — 그리고 아직 붙은 적 없는 pane("replaying"으로 시작한 것)은 실패 오버레이로
   * 바꾼다. 놔두면 어느 pipeline도 없는 pane이 "기록 재생 중…"에 영원히 남는다.
   */
  private failUnattachedReplayPanes(message: string): void {
    this.restorePromise = null;
    if (this.disposed) return;
    this.toast(message);
    const panes = useWorkbenchStore.getState().panes;
    for (const pane of Object.values(panes)) {
      if (pane.phase !== "replaying" || !pane.viewId) continue;
      if (this.pipelines.has(pane.viewId)) continue; // 이미 붙어 재생 중 — pipeline이 상태를 바꾼다
      useWorkbenchStore.getState().panePhase(pane.leafId, "failed", message);
    }
  }

  /** Startup must await discovery before deciding to create a new shell. */
  restoreWorkspace(): Promise<void> {
    if (!this.restorePromise) {
      this.restorePromise = this.restoreExistingSessions();
      // 실패한 promise를 cache에 두면 이후 호출이 영원히 같은 실패만 돌려준다 —
      // 실패를 알리고 cache를 비운다(붙지 못한 pane은 실패 오버레이로).
      this.restorePromise.catch((error) => {
        this.failUnattachedReplayPanes(errorMessage(error, t("terminal.session.restoreFailed")));
      });
    }
    return this.restorePromise;
  }

  private async restoreExistingSessions(): Promise<void> {
    const savedPanes = Object.values(useWorkbenchStore.getState().panes);
    const snapshot = await this.deps.client.systemSnapshot();
    if (this.disposed) return;
    useWorkbenchStore.getState().applySnapshot(snapshot);
    this.refreshPaneAgents();
    resourceHistory.push(snapshot.host);
    // 재시작 전 개입 알림 복원(W1-5): 놓친 승인 요청을 알림센터에서 다시 본다.
    try {
      const notices = await this.deps.client.interventionList();
      const notifications = useNotificationStore.getState();
      for (const notice of [...notices].reverse()) {
        notifications.push({
          id: notice.report_id,
          kind: "intervention",
          title: notice.title,
          detail: notice.detail ?? null,
          at: notice.reported_at,
          sessionId: this.matchSessionByHint(notice.session_hint ?? null),
          interventionKind: notice.kind,
          acknowledged: true, // 이미 사용자가 봤을 수 있는 과거 항목 — 읽음으로 시작
        });
      }
    } catch {
      // 구 데몬은 메서드가 없다 — 알림센터는 비어 시작한다.
    }
    if (this.disposed) return;
    const active = snapshot.workloads.filter(w => ["STARTING", "RUNNING", "STOPPING", "DRAINING"].includes(w.state));
    const byWorkload = new Map(active.map(w => [w.workload_id, w]));
    const attach: Array<{ leafId: string; sessionId: string; workloadId: string }> = [];
    for (const pane of savedPanes) {
      if (this.disposed) return;
      if (this.pipelines.has(pane.viewId)) continue;
      if ((!pane.sessionId || !pane.workloadId) && !pane.resume) continue;
      const workload = pane.workloadId ? byWorkload.get(pane.workloadId) : null;
      if (!workload || (workload.session_id && workload.session_id !== pane.sessionId)) {
        const resume = await this.findResumableSession(pane.workloadId, pane.sessionId, pane.resume);
        if (this.disposed) return;
        if (useWorkbenchStore.getState().panes[pane.leafId]?.viewId !== pane.viewId) continue;
        this.disposeView(pane.viewId);
        if (resume) {
          // 앱 복원은 대화와 자리를 보존한다. 연결·재실행할 때 같은 ID로 연다.
          const viewId = this.uuid();
          useWorkbenchStore.setState(s => {
            const current = s.panes[pane.leafId];
            if (!current) return s;
            return {
              panes: {
                ...s.panes,
                [pane.leafId]: {
                  ...current,
                  viewId,
                  phase: "exited" as PanePhase,
                  exit: null,
                  agent: null,
                  resume,
                },
              },
            };
          });
          useWorkbenchStore.getState().patchLeaf(pane.leafId, { view_id: viewId });
          continue;
        }
        // 일반 터미널도 보관된 출력 저널을 다시 붙여 읽을 수 있다.
        if (pane.sessionId && pane.workloadId) attach.push({
          leafId: pane.leafId, sessionId: pane.sessionId, workloadId: pane.workloadId,
        });
        continue;
      }
      if (pane.sessionId && pane.workloadId) {
        attach.push({ leafId: pane.leafId, sessionId: pane.sessionId, workloadId: pane.workloadId });
      }
    }
    const state = useWorkbenchStore.getState();
    const known = new Set(Object.values(state.panes).map(p => p.sessionId));
    const activeTabId = state.activeTabId, focusedLeafId = state.focusedLeafId;
    // 저장된 배치에 없는 살아 있는 세션(04-ui §4): 세션마다 탭 하나가 아니라
    // 작업 cwd별로 한 탭에 모은다(04-ui §2-5). 복원 시점에는 git 최상위를
    // 아직 조회하지 않았으므로 cwd가 유일한 프로젝트 근거다.
    const orphans = active.filter((w) => w.session_id && !known.has(w.session_id));
    for (const group of groupOrphansByCwd(orphans)) {
      if (this.disposed) return;
      const leaves: SplitNode[] = [];
      const panes: Record<string, PaneMeta> = {};
      for (const workload of group.workloads) {
        const sessionId = workload.session_id!;
        const leafId = this.uuid(), viewId = this.uuid();
        leaves.push(makeLeaf(leafId, viewId, sessionId));
        panes[leafId] = this.makePaneMeta(leafId, viewId, sessionId, workload.workload_id, workload.title, workload.cwd);
        known.add(sessionId);
        attach.push({ leafId, sessionId, workloadId: workload.workload_id });
      }
      const tabId = this.uuid();
      const root = gridLayout(leaves, () => this.uuid());
      useWorkbenchStore.setState(s => ({
        tabs: [...s.tabs, { kind: "terminal" as const, id: tabId, title: group.title, root }],
        panes: { ...s.panes, ...panes },
        activeTabId: activeTabId ?? s.activeTabId ?? tabId,
        focusedLeafId: focusedLeafId ?? s.focusedLeafId ?? leaves[0].id,
      }));
    }
    // Old daemons omit session IDs. Cached references still work, but absence
    // of IDs is not evidence that there are no existing sessions.
    if (active.length > 0 && Object.keys(useWorkbenchStore.getState().panes).length === 0) {
      throw new Error(t("terminal.session.restoreUnavailable"));
    }
    for (const pane of attach) {
      if (!useWorkbenchStore.getState().panes[pane.leafId]) continue;
      this.liveSessions.add(pane.sessionId);
      this.workloadSession.set(pane.workloadId, pane.sessionId);
      useWorkbenchStore.getState().panePhase(pane.leafId, "replaying");
    }
    // 보이는 탭을 먼저 붙인다: 모든 pane의 재생이 한꺼번에 메인 스레드를 나눠 쓰면
    // 지금 보는 화면이 가장 늦게 뜬다. 나머지는 그 재생이 끝나거나(상한 있음)
    // 그 pane이 보이게 될 때 붙인다.
    const visible = this.activeTabLeafIds();
    const foreground = attach.filter(pane => visible.has(pane.leafId));
    const background = foreground.length > 0 ? attach.filter(pane => !visible.has(pane.leafId)) : [];
    for (const pane of background) this.deferredAttaches.set(pane.leafId, pane);
    for (const pane of foreground) this.foregroundReplays.add(pane.leafId);
    // 복원 중 새로 만든 pane(고아 탭)도 이 시점엔 존재한다 — 배경 tint를
    // 계산해 둔다(뷰는 attach 때 생기고 registry가 저장된 값을 적용한다).
    this.applyAllPaneBackgrounds();
    for (const pane of foreground.length > 0 ? foreground : attach) {
      if (this.disposed) return;
      if (!useWorkbenchStore.getState().panes[pane.leafId]) {
        this.foregroundReplayDone(pane.leafId);
        continue;
      }
      await this.attachPane(pane.leafId, pane.sessionId, pane.workloadId);
    }
    if (this.disposed) return;
    this.pruneReplaySnapshots();
    if (this.deferredAttaches.size === 0) return;
    if (this.foregroundReplays.size === 0) {
      void this.flushDeferredAttaches();
    } else {
      this.deferredAttachTimer = setTimeout(() => void this.flushDeferredAttaches(), DEFERRED_ATTACH_MAX_WAIT_MS);
    }
  }

  /** 활성 탭(터미널 탭)의 pane leaf id들. */
  private activeTabLeafIds(): Set<string> {
    const state = useWorkbenchStore.getState();
    const tab = state.tabs.find(candidate => candidate.id === state.activeTabId);
    return new Set(tab?.kind === "terminal" ? listLeaves(tab.root).map(leaf => leaf.id) : []);
  }

  /** 활성 탭 pane 하나의 재생이 끝났다(live·exited·실패). 모두 끝나면 미룬 연결을 시작한다. */
  private foregroundReplayDone(leafId: string): void {
    if (!this.foregroundReplays.delete(leafId)) return;
    if (this.foregroundReplays.size === 0 && this.deferredAttaches.size > 0) void this.flushDeferredAttaches();
  }

  /** 미룬 pane 연결을 차례로 붙인다(중복 호출은 진행 중인 것을 기다린다). */
  private flushDeferredAttaches(): Promise<void> {
    if (this.deferredAttachTimer) clearTimeout(this.deferredAttachTimer);
    this.deferredAttachTimer = null;
    this.foregroundReplays.clear();
    if (this.deferredAttachRun) return this.deferredAttachRun;
    this.deferredAttachRun = (async () => {
      while (this.deferredAttaches.size > 0 && !this.disposed) {
        const [leafId, item] = this.deferredAttaches.entries().next().value!;
        this.deferredAttaches.delete(leafId);
        try {
          await this.attachDeferred(item);
        } catch {
          // 한 pane 실패가 나머지 미룬 pane을 붙잡지 않게 한다 — attachPane이
          // 스스로 실패를 pane에 남기므로 여기서는 삼키고 다음으로 간다.
        }
      }
    })().finally(() => {
      this.deferredAttachRun = null;
    });
    return this.deferredAttachRun;
  }

  /** 미룬 pane이 보이게 됐다 — 그 pane만 바로 붙인다. */
  private attachDeferredView(viewId: string): void {
    const panes = useWorkbenchStore.getState().panes;
    for (const [leafId, item] of this.deferredAttaches) {
      if (panes[leafId]?.viewId !== viewId) continue;
      this.deferredAttaches.delete(leafId);
      void this.attachDeferred(item);
      return;
    }
  }

  /** 기다리는 사이 닫혔거나 다른 세션으로 바뀌었거나 이미 붙은 pane은 건너뛴다. */
  private async attachDeferred(item: { leafId: string; sessionId: string; workloadId: string }): Promise<void> {
    if (this.disposed) return;
    const pane = useWorkbenchStore.getState().panes[item.leafId];
    if (!pane || pane.sessionId !== item.sessionId || this.pipelines.has(pane.viewId)) return;
    await this.attachPane(item.leafId, item.sessionId, item.workloadId);
  }

  /**
   * 재생이 막힌 pane을 다시 붙인다 — 스냅샷 없이 전체 재생으로 물러난다. 막힘의
   * 원인(저널 앞부분 잘림·전송 결함·브리지 경로 손실 어느 쪽이든)과 무관하게
   * 유일한 회복은 새 epoch으로 다시 붙는 것이다. 두 번 넘게 반복되면 그대로
   * 실패 오버레이(재시도 있음)로 끝낸다 — 무한 재접속을 돌리지 않는다.
   */
  private async recoverStalledReplay(viewId: string, leafId: string, sessionId: string): Promise<void> {
    if (this.disposed) return;
    const pipeline = this.pipelines.get(viewId);
    const entry = this.deps.registry.get(viewId);
    const ref = this.sessionIndex.get(sessionId);
    // 스토어에서 사라진 pane은 보여 줄 곳이 없다 — 조용히 넘어간다.
    if (!useWorkbenchStore.getState().panes[leafId]) return;
    if (!pipeline || !entry || !ref || ref.viewId !== viewId) {
      // pane은 살아 있는데 다시 붙을 pipeline·경로를 잃었다 — 되돌릴 수 없는
      // 막힘이므로 실패 오버레이로 끝낸다. 조용히 돌아가면 "기록 재생 중…"
      // 오버레이에 영원히 남는다.
      useWorkbenchStore.getState().panePhase(leafId, "failed", t("terminal.session.replayStalled"));
      this.foregroundReplayDone(leafId);
      return;
    }
    // 이미 재접속이 진행 중이다 — 같은 막힘으로 strikes를 중복으로 올리지 않는다.
    if (pipeline.isAttachPending) return;
    const strikes = (this.replayStallStrikes.get(leafId) ?? 0) + 1;
    this.replayStallStrikes.set(leafId, strikes);
    if (strikes > 2) {
      // 회복 불능으로 끝낼 때는 붙어 있는 pipeline과 데몬 쪽 view도 치운다
      // (retryPane과 같은 정리) — 남겨 두면 pipeline은 종료까지 이 세션의
      // 흐름 크레딧을 붙잡은 반쪽짜리 연결로 남는다.
      pipeline.dispose();
      this.pipelines.delete(ref.viewId);
      if (this.sessionIndex.get(sessionId) === ref) this.sessionIndex.delete(sessionId);
      this.releaseSessionRoute(sessionId, ref.viewId);
      // 나중에 수동 재시도가 깨끗한 상태에서 시작한다.
      this.replayStallStrikes.delete(leafId);
      this.clearReattachBackoff(leafId);
      useWorkbenchStore.getState().panePhase(leafId, "failed", t("terminal.session.replayStalled"));
      this.foregroundReplayDone(leafId);
      return;
    }
    useWorkbenchStore.getState().panePhase(leafId, "replaying");
    try {
      await pipeline.attach(true, null, {
        maxReplayBytes: this.replayBudget(sessionId, useWorkbenchStore.getState().panes[leafId]?.workloadId ?? null),
      });
      if (!this.disposed) entry.refit();
    } catch (error) {
      // 다시 붙는 사이 수동 재시도가 새 view를 붙였으면 이 실패로 덮지 않는다(attachPane과 같다).
      if (!this.disposed && useWorkbenchStore.getState().panes[leafId] && this.pipelines.get(viewId) === pipeline) {
        useWorkbenchStore.getState().panePhase(
          leafId,
          "failed",
          errorMessage(error, t("terminal.session.attachFailed")),
        );
        this.foregroundReplayDone(leafId);
      }
    }
  }

  /** 복구는 최근 목록이나 cwd가 아니라 저장된 실행의 식별자로 조회한다. */
  private async findResumableSession(
    workloadId: string | null,
    sessionId: string | null,
    saved?: AgentResumeInfo | null,
  ): Promise<AgentResumeInfo | null> {
    const fallback = decodeResumeInfo(saved);
    if (fallback && this.activeAgentSession(fallback, workloadId)) return null;
    if (!workloadId && !sessionId) return fallback;
    let records: AgentSessionRecord[];
    try {
      records = await this.deps.client.agentSessionList({
        workload_id: workloadId,
        pty_session_id: sessionId,
        limit: 1,
      });
    } catch {
      return fallback; // 데몬 기록이 없어도 관찰해 저장한 정확한 ID를 사용한다.
    }
    // 구 데몬이 새 필터를 무시하더라도 다른 대화를 복구하지 않는다.
    const record = records.find(r =>
      (workloadId != null && r.workload_id === workloadId) ||
      (sessionId != null && r.pty_session_id === sessionId));
    return record ? (!record.active ? resumeInfoFrom(record) : null) : fallback;
  }

  /**
   * 끝난 작업을 어떻게 되살릴지 정한다. 이 작업·PTY의 에이전트 기록(없으면 pane에
   * 저장된 마킹)이 있으면 그 대화다: 데몬이 active로 알리거나(계약상 복구 대신 그
   * pane으로 이동) UI가 같은 대화를 실행 중인 작업을 알면 그리로 가고, 아니면 이어서
   * 연다. 기록이 없으면 일반 터미널이다.
   */
  private async findRecoveryTarget(
    workloadId: string,
    sessionId: string | null,
    saved?: AgentResumeInfo | null,
  ): Promise<RecoveryTarget> {
    let record: AgentSessionRecord | undefined;
    try {
      const records = await this.deps.client.agentSessionList({
        workload_id: workloadId,
        pty_session_id: sessionId,
        limit: 1,
      });
      // 구 데몬이 새 필터를 무시하더라도 다른 대화를 복구하지 않는다.
      record = records.find(r => r.workload_id === workloadId || (sessionId != null && r.pty_session_id === sessionId));
    } catch {
      record = undefined; // 데몬 기록이 없어도 관찰해 저장한 정확한 ID를 사용한다.
    }
    const info = record ? resumeInfoFrom(record) : decodeResumeInfo(saved);
    const live = info ? this.activeAgentSession(info, workloadId) : null;
    if (live || record?.active) return { kind: "running", live };
    return info ? { kind: "resume", info } : { kind: "shell" };
  }

  /** 다른 pane/워크로드가 이미 같은 대화를 실행 중이면 중복 실행하지 않는다 — 그 실행을 돌려준다. */
  private activeAgentSession(info: AgentResumeInfo, ignoredWorkloadId: string | null = null): LiveConversation | null {
    const state = useWorkbenchStore.getState();
    const workload = state.workloads.find(w => w.workload_id !== ignoredWorkloadId && !isFinishedWorkload(w.state) &&
      w.agent?.agent === info.agent && w.agent.session_id === info.agentSessionId);
    if (workload) return { workloadId: workload.workload_id, sessionId: workload.session_id ?? null, leafId: null };
    const pane = Object.values(state.panes).find(p => (p.phase === "starting" ||
      (p.phase !== "exited" && p.phase !== "failed" && p.sessionId && this.liveSessions.has(p.sessionId))) &&
      p.resume?.agent === info.agent && p.resume.agentSessionId === info.agentSessionId);
    return pane ? { workloadId: pane.workloadId, sessionId: pane.sessionId, leafId: pane.leafId } : null;
  }

  /** 실행 중인 대화로 간다. 어느 창에도 붙어 있지 않으면(창만 닫기) 새 창으로 연결한다. */
  private focusLiveConversation(live: LiveConversation): void {
    const pane = Object.values(useWorkbenchStore.getState().panes).find(p =>
      (live.leafId !== null && p.leafId === live.leafId) || (live.sessionId !== null && p.sessionId === live.sessionId));
    if (pane) {
      this.focusLeaf(pane.leafId);
      return;
    }
    if (live.sessionId) this.attachSessionToNewPane(live.sessionId, live.workloadId);
  }

  /**
   * 조회 중 닫히거나 새 실행으로 바뀐 pane에는 이전 세션 정보를 쓰지 않는다.
   * `closeWhenNoResume`은 종료 시점에 한 번 정한 판단이다(04-ui §5-1) — 여기서
   * 다시 계산하지 않는다. 조회 사이에 들어온 작업 요약이 판단을 뒤집지
   * 않게 하려는 것이다.
   */
  private async updateExitedPaneResume(leafId: string, closeWhenNoResume = false): Promise<void> {
    const pane = useWorkbenchStore.getState().panes[leafId];
    if (!pane || pane.phase !== "exited") return;
    const resume = await this.findResumableSession(pane.workloadId, pane.sessionId, pane.resume);
    if (this.disposed) return;
    const current = useWorkbenchStore.getState().panes[leafId];
    if (current?.viewId !== pane.viewId || current.sessionId !== pane.sessionId ||
      current.workloadId !== pane.workloadId || current.phase !== "exited") return;
    useWorkbenchStore.getState().paneResume(leafId, resume);
    // 에이전트로 보였지만 이어서 열 기록이 없는 창(기록 없음·같은 대화가
    // 다른 창에서 실행 중)은 일반 터미널과 같이 그 자리에서 닫는다.
    if (!resume && closeWhenNoResume) void this.autoCloseExitedPane(leafId);
  }

  /**
   * 관리 실행(AI 작업·빠른 실행처럼 한 번 돌고 끝나는 작업)의 창인가.
   * 그 창의 출력은 사용자가 보려고 실행한 결과라 정상 종료에서도
   * 자동으로 지우지 않는다 — 자동 닫기는 사용자가 직접 쓰는 셸만의 규칙이다.
   */
  private isManagedPane(pane: PaneMeta | null): boolean {
    if (!pane?.workloadId) return false;
    const workload = useWorkbenchStore.getState().workloads
      .find((candidate) => candidate.workload_id === pane.workloadId);
    return workload?.mode === "managed";
  }

  /**
   * 정상 종료로 끝난 창을 그 자리에서 지운다(04-ui §5-1). 프로세스는
   * 이미 스스로 끝났으므로 취소는 보내지 않는다(창만 닫기) — 데몬이
   * 마무리하는 중에 끼어들면 잘 끝난 작업이 최근 종료 목록에 취소로
   * 남을 수 있다. 그 탭의 마지막 창이었으면 빈 탭을 남기지 않고 탭까지
   * 닫는다(마지막 탭이면 빈 프로젝트 화면 — 새 셸을 대신 띄우지 않는다).
   */
  private async autoCloseExitedPane(leafId: string): Promise<void> {
    const tabId = useWorkbenchStore.getState().tabs
      .find((tab) => tab.kind === "terminal" && findLeaf(tab.root, leafId) !== null)?.id ?? null;
    await this.closePane(leafId, false);
    if (this.disposed || tabId === null) return;
    const tab = useWorkbenchStore.getState().tabs.find((candidate) => candidate.id === tabId);
    if (tab?.kind === "terminal" && !tab.root) useWorkbenchStore.getState().closeTab(tabId);
  }

  stop(): void {
    this.eventSub?.dispose();
    this.eventSub = null;
    this.focusSub?.();
    this.focusSub = null;
    if (this.transportTimer) clearInterval(this.transportTimer);
    this.transportTimer = null;
    if (this.snapshotTimer) clearInterval(this.snapshotTimer);
    this.snapshotTimer = null;
    this.visibilitySub?.();
    this.visibilitySub = null;
    for (const leafId of [...this.healthyTimers.keys()]) this.cancelHealthyTimer(leafId);
    for (const leafId of [...this.reattachBackoff.keys()]) this.clearReattachBackoff(leafId);
  }

  /**
   * focus 보고(08-pressure-relief §1): "지금 보고 있는 세션"이 바뀔 때만
   * 한 번 보낸다. focus한 pane이 없거나 아직 세션이 붙지 않았으면 null이다
   * (스토어 구독이 세션 배정 시점의 변화도 같이 잡는다).
   *
   * 방금 끝난 세션을 보고하는 경합은 정상 경로이고 데몬은 그때
   * INVALID_ARGUMENT로 거절한다 — 실패는 로그로만 남기고 UI로 올리지 않는다.
   */
  private reportFocus(): void {
    if (this.disposed) return;
    const store = useWorkbenchStore.getState();
    const sessionId =
      (store.focusedLeafId ? store.panes[store.focusedLeafId]?.sessionId : null) ?? null;
    if (sessionId === this.reportedFocusSessionId) return;
    this.reportedFocusSessionId = sessionId;
    // Promise 체인으로 감싸 sessionFocus가 없는 구형/축약 클라이언트의
    // 동기 예외까지 같은 자리에서 흡수한다.
    void Promise.resolve()
      .then(() => this.deps.client.sessionFocus({ session_id: sessionId }))
      .catch((error: unknown) => {
        console.debug("session.focus failed", error);
      });
  }

  /**
   * React StrictMode runs effect setup → cleanup → setup once in development.
   * Deferring full disposal by one task lets the second setup cancel it, while
   * a real HMR/unmount still releases every stale pipeline and xterm instance.
   */
  scheduleDispose(): void {
    this.stop();
    if (this.deferredDisposeTimer || this.disposed) return;
    this.deferredDisposeTimer = setTimeout(() => {
      this.deferredDisposeTimer = null;
      this.dispose();
    }, 0);
  }

  /** Frontend-only teardown; daemon workloads and PTYs intentionally survive. */
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.stop();
    // 표시 관찰자 해제: 이후 disposeAll의 unmount 통보가 폐기 중인 pipeline으로
    // 흘러들지 않게 한다(teardown에서 리스너를 비운다).
    this.deps.registry.setVisibilityListener(null);
    if (this.deferredDisposeTimer) clearTimeout(this.deferredDisposeTimer);
    this.deferredDisposeTimer = null;
    if (this.toastTimer) clearTimeout(this.toastTimer);
    this.toastTimer = null;
    if (this.deferredAttachTimer) clearTimeout(this.deferredAttachTimer);
    this.deferredAttachTimer = null;
    this.deferredAttaches.clear();
    this.foregroundReplays.clear();
    this.snapshotMarks.clear();
    for (const [viewId, pipeline] of this.pipelines) {
      pipeline.dispose();
      this.releaseSessionRoute(pipeline.sessionId, viewId);
      this.deps.registry.setFitHandler(viewId, null);
    }
    this.pipelines.clear();
    this.sessionIndex.clear();
    this.workloadSession.clear();
    this.paneSizes.clear();
    this.liveSessions.clear();
    this.detachedPendingLaunches.clear();
    this.pendingShellRecoveries.clear();
    this.recoveryOrigins.clear();
    this.patternTails.clear();
    this.deps.registry.disposeAll();
  }

  private async checkTransport(): Promise<void> {
    const statusFn = this.deps.client.transportStatus;
    const reconnect = this.deps.client.reconnectTransport;
    if (!statusFn || !reconnect || this.disposed || this.transportCheckInFlight) return;
    this.transportCheckInFlight = true;
    try {
      const status = await statusFn.call(this.deps.client);
      if (this.disposed) return;
      const generationChanged =
        this.transportGeneration !== null && status.generation !== this.transportGeneration;
      // 재접속이 실제로 시작되는 pane만 replaying으로 바뀐다 — 연결 흔들림에
      // 모든 터미널이 한 번에 꺼지는 깜빡임을 없앤다. 각 pane의 단계는
      // recoverAttachedPanes의 attach가 시작될 때 그 pane의 onModeChange가 바꾼다.
      const unhealthy = !status.controlAlive || !status.dataAlive;
      if (unhealthy || generationChanged) {
        // 복구 폭주 방지: 잇단 복구는 backoff로 띄운다. 미룰 때는 기준
        // generation·상태를 갱신하지 않아 다음 점검이 같은 판정을 다시 한다.
        if (!this.transportRecoveryDue()) return;
        if (unhealthy) {
          await reconnect.call(this.deps.client);
          const reconnected = await statusFn.call(this.deps.client);
          this.transportGeneration = reconnected.generation;
        } else {
          this.transportGeneration = status.generation;
        }
        await this.recoverAttachedPanes();
      } else {
        this.transportGeneration = status.generation;
      }
    } catch (error) {
      if (!this.disposed) this.markAttachedPanesFailed(error);
    } finally {
      this.transportCheckInFlight = false;
    }
  }

  /**
   * 지금 전송 복구를 시작해도 되는가. 마지막 복구 뒤 TRANSPORT_HEALTHY_MS가
   * 지났으면 연속 횟수를 0으로 되돌린다(첫 복구는 즉시). 잇단 복구는
   * 2초·4초·8초…(상한 60초) 간격을 둔다 — 복구 자체가 데몬을 다시 끊는
   * 순환에서 모든 pane이 2초마다 reset+재생으로 깜빡이지 않게.
   */
  private transportRecoveryDue(): boolean {
    const now = Date.now();
    if (this.transportRecoverAt > 0 && now - this.transportRecoverAt >= TRANSPORT_HEALTHY_MS) {
      this.transportRecoverStreak = 0;
    }
    const delay = this.transportRecoverStreak === 0
      ? 0
      : Math.min(
          TRANSPORT_RECOVER_BACKOFF_MAX_MS,
          TRANSPORT_RECOVER_BACKOFF_BASE_MS * 2 ** (this.transportRecoverStreak - 1),
        );
    if (now - this.transportRecoverAt < delay) return false;
    this.transportRecoverAt = now;
    this.transportRecoverStreak += 1;
    return true;
  }

  private markAttachedPanesFailed(error: unknown): void {
    const message = errorMessage(error, t("terminal.session.attachFailed"));
    const store = useWorkbenchStore.getState();
    for (const { leafId } of this.sessionIndex.values()) {
      // 끝난 창은 저널을 다시 읽으려고 sessionIndex에 남아 있을 뿐이다 — 연결이 끊겼다고
      // 종료 오버레이("이어서 열기"·"새 세션으로 다시 시작")를 실패로 덮지 않는다.
      const pane = store.panes[leafId];
      if (pane && pane.phase !== "exited") store.panePhase(leafId, "failed", message);
    }
  }

  /**
   * 데몬 강제 재접속을 backoff로 예약한다. 같은 pane의 첫 알림은 즉시, 잇단
   * 알림은 1·2·4…초(상한 15초) 뒤에 한 번만 붙는다(사이의 알림은 합친다).
   * REATTACH_STORM_WINDOW_MS 안에 REATTACH_STORM_LIMIT를 넘기면 재생을
   * 되풀이하지 않고 실패 오버레이로 끝낸다 — 수동 재시도는 깨끗하게 시작한다.
   * live로 REPLAY_HEALTHY_MS를 버티면 횟수를 지운다(armHealthyTimer).
   */
  private scheduleReattach(ref: { viewId: string; leafId: string }, sessionId: string): void {
    if (this.disposed) return;
    const now = Date.now();
    let state = this.reattachBackoff.get(ref.leafId);
    if (!state || now - state.windowStart >= REATTACH_STORM_WINDOW_MS) {
      if (state?.timer) clearTimeout(state.timer);
      state = { count: 0, windowStart: now, timer: null };
      this.reattachBackoff.set(ref.leafId, state);
    }
    // 데몬이 view를 뗐으니 pipeline이 live로 보여도 건강하지 않다 — live로
    // 버틴 시간 재기를 접어 backoff 횟수가 기다리는 사이 지워지지 않게 한다.
    this.cancelHealthyTimer(ref.leafId);
    // 이미 예약된 재접속이 이 알림도 흡수한다 — 알림마다 다시 붙지 않는다.
    if (state.timer !== null) return;
    state.count += 1;
    if (state.count > REATTACH_STORM_LIMIT) {
      this.failLoopingPane(ref, sessionId);
      return;
    }
    // 기다리는 동안 입력이 닿지 않는 이유를 pane에 보인다.
    if (useWorkbenchStore.getState().panes[ref.leafId]) {
      useWorkbenchStore.getState().panePhase(ref.leafId, "replaying");
    }
    const delay = state.count <= 1
      ? 0
      : Math.min(REATTACH_BACKOFF_MAX_MS, REATTACH_BACKOFF_BASE_MS * 2 ** (state.count - 2));
    const run = () => {
      const current = this.reattachBackoff.get(ref.leafId);
      if (current) current.timer = null;
      if (this.disposed || this.sessionIndex.get(sessionId) !== ref) return;
      void this.reattachPane(ref);
    };
    if (delay === 0) {
      run();
      return;
    }
    state.timer = setTimeout(run, delay);
  }

  /**
   * 입력 전송이 실패했다. 데몬이 view를 이미 뗐는데 재접속 알림을 놓친 pane은
   * live로 보이면서 모든 키가 STALE_EPOCH 등으로 거절된다 — 아무도 알리지 않으면
   * 앱을 재시작할 때까지 입력이 죽는다. 그런 거절이면 그 pane을 다시 붙인다.
   */
  private recoverFromInputFailure(viewId: string, sessionId: string, error: Error): void {
    if (this.disposed) return;
    const rejection = classifyInputRejection(error);
    if (rejection !== "detached") {
      // 다시 붙어도 낫지 않는 거절: 버려진 입력의 원인을 pane에 보이고(멈춘
      // 프로그램·가드 일시정지) 로그에 남긴다 — 예전에는 흔적 없이 사라졌다.
      const code = (error as { code?: unknown }).code;
      if (rejection === "other") {
        console.warn("terminal input rejected", { sessionId, code, message: error.message });
      } else {
        if (inputIssueOf(sessionId) !== rejection) {
          console.warn("terminal input rejected", { sessionId, code, reason: rejection, message: error.message });
        }
        setInputIssue(sessionId, rejection);
      }
      return;
    }
    const ref = this.sessionIndex.get(sessionId);
    if (!ref || ref.viewId !== viewId) return;
    if (this.pipelines.get(viewId)?.isAttachPending) return;
    this.scheduleReattach(ref, sessionId);
  }

  /**
   * 재접속이 폭주한 pane: pipeline과 데몬 쪽 view 경로를 치우고 실패
   * 오버레이로 끝낸다(recoverStalledReplay의 포기 경로와 같은 정리).
   */
  private failLoopingPane(ref: { viewId: string; leafId: string }, sessionId: string): void {
    this.pipelines.get(ref.viewId)?.dispose();
    this.pipelines.delete(ref.viewId);
    if (this.sessionIndex.get(sessionId) === ref) this.sessionIndex.delete(sessionId);
    this.releaseSessionRoute(sessionId, ref.viewId);
    this.clearReattachBackoff(ref.leafId);
    this.cancelHealthyTimer(ref.leafId);
    this.replayStallStrikes.delete(ref.leafId);
    if (useWorkbenchStore.getState().panes[ref.leafId]) {
      useWorkbenchStore.getState().panePhase(ref.leafId, "failed", t("terminal.session.replayLooping"));
    }
    this.foregroundReplayDone(ref.leafId);
  }

  private clearReattachBackoff(leafId: string): void {
    const state = this.reattachBackoff.get(leafId);
    if (state?.timer) clearTimeout(state.timer);
    this.reattachBackoff.delete(leafId);
  }

  /** live로 REPLAY_HEALTHY_MS를 버티면 막힘 strikes와 재접속 backoff를 지운다. */
  private armHealthyTimer(leafId: string): void {
    this.cancelHealthyTimer(leafId);
    this.healthyTimers.set(
      leafId,
      setTimeout(() => {
        this.healthyTimers.delete(leafId);
        this.replayStallStrikes.delete(leafId);
        this.clearReattachBackoff(leafId);
      }, REPLAY_HEALTHY_MS),
    );
  }

  private cancelHealthyTimer(leafId: string): void {
    const timer = this.healthyTimers.get(leafId);
    if (timer !== undefined) clearTimeout(timer);
    this.healthyTimers.delete(leafId);
  }

  /**
   * pane 하나를 같은 pipeline으로 다시 붙인다(데몬이 view를 떼어낸 뒤).
   * 재접속은 언제나 새 epoch의 재생이므로 화면을 먼저 비운다.
   */
  private async reattachPane(ref: { viewId: string; leafId: string }): Promise<void> {
    const pipeline = this.pipelines.get(ref.viewId);
    const entry = this.deps.registry.get(ref.viewId);
    const pane = useWorkbenchStore.getState().panes[ref.leafId];
    if (!pipeline || !entry || !pane) return;
    // 재접속 직전 화면을 떠 두면 데몬이 받아들일 때 그 뒤만 재생한다.
    const snapshot = this.currentSnapshot(ref.viewId);
    useWorkbenchStore.getState().panePhase(ref.leafId, "replaying");
    try {
      await pipeline.attach(true, snapshot, {
        maxReplayBytes: this.replayBudget(pipeline.sessionId, pane.workloadId),
      });
      if (!this.disposed) entry.refit();
    } catch (error) {
      // 다시 붙는 사이 수동 재시도가 새 view를 붙였으면 이 실패로 덮지 않는다(attachPane과 같다).
      if (!this.disposed && useWorkbenchStore.getState().panes[ref.leafId] && this.pipelines.get(ref.viewId) === pipeline) {
        useWorkbenchStore
          .getState()
          .panePhase(ref.leafId, "failed", errorMessage(error, t("terminal.session.attachFailed")));
      }
    }
  }

  private recoverAttachedPanes(): Promise<void> {
    if (this.transportRecovery) return this.transportRecovery;
    this.transportRecovery = (async () => {
      // Revisions restart at 1 in a new daemon. Refresh the authoritative list.
      const snapshot = await this.deps.client.systemSnapshot();
      if (this.disposed) return;
      useWorkbenchStore.setState({ revision: 0 });
      useWorkbenchStore.getState().applySnapshot(snapshot);
      for (const [sessionId, ref] of [...this.sessionIndex]) {
        if (this.disposed) return;
        const pipeline = this.pipelines.get(ref.viewId);
        const entry = this.deps.registry.get(ref.viewId);
        if (!pipeline || !entry || !useWorkbenchStore.getState().panes[ref.leafId]) continue;
        const workload = snapshot.workloads.find(w => w.session_id === sessionId);
        if (workload?.last_error_code === "DAEMON_RESTART") {
          this.liveSessions.delete(sessionId);
          this.daemonRestartedSessions.add(sessionId);
          useWorkbenchStore.getState().paneAgent(ref.leafId, null);
          useWorkbenchStore.getState().panePhase(ref.leafId, "exited", t("terminal.session.daemonRestarted"));
          await this.updateExitedPaneResume(ref.leafId);
          if (this.disposed || this.sessionIndex.get(sessionId) !== ref) continue;
          if (!useWorkbenchStore.getState().panes[ref.leafId]?.resume) {
            try {
              await pipeline.attach(true, this.currentSnapshot(ref.viewId), {
                maxReplayBytes: this.replayBudget(sessionId, workload?.workload_id ?? null),
              });
              entry.refit();
            }
            catch (error) {
              if (!this.disposed) useWorkbenchStore.getState().panePhase(ref.leafId, "failed", errorMessage(error, t("terminal.session.attachFailed")));
            }
            continue;
          }
          pipeline.dispose();
          this.pipelines.delete(ref.viewId);
          this.sessionIndex.delete(sessionId);
          this.releaseSessionRoute(sessionId, ref.viewId);
          clearTerminalActivity(sessionId);
          setInputIssue(sessionId, null);
          continue;
        }
        try {
          await pipeline.attach(true, this.currentSnapshot(ref.viewId), {
            maxReplayBytes: this.replayBudget(sessionId, workload?.workload_id ?? null),
          });
          if (!this.disposed) entry.refit();
        } catch (error) {
          if (!this.disposed && this.sessionIndex.get(sessionId) === ref) {
            useWorkbenchStore
              .getState()
              .panePhase(ref.leafId, "failed", errorMessage(error, t("terminal.session.attachFailed")));
          }
        }
      }
      // 새 제어 연결은 focus를 모른다 — 기억을 비우고 현재 값을 다시 보낸다.
      this.reportedFocusSessionId = null;
      this.reportFocus();
    })().finally(() => {
      this.transportRecovery = null;
    });
    return this.transportRecovery;
  }

  handleEvent(event: DaemonEvent): void {
    if (this.disposed) return;
    const store = useWorkbenchStore.getState();
    switch (event.kind) {
      case "session.output": {
        const ref = this.sessionIndex.get(event.payload.session_id);
        if (ref) this.pipelines.get(ref.viewId)?.handleOutput(event.payload);
        break;
      }
      case "session.resize_applied": {
        const ref = this.sessionIndex.get(event.payload.session_id);
        if (ref) this.pipelines.get(ref.viewId)?.handleResizeApplied(event.payload);
        break;
      }
      case "session.flow_blocked": {
        const ref = this.sessionIndex.get(event.payload.session_id);
        if (ref) store.paneFlowBlocked(ref.leafId, event.payload.blocked);
        break;
      }
      case "session.exited": {
        clearTerminalActivity(event.payload.session_id);
        setInputIssue(event.payload.session_id, null);
        this.liveSessions.delete(event.payload.session_id);
        const ref = this.sessionIndex.get(event.payload.session_id);
        if (ref) {
          // 파이프라인이 exited로 넘어가면 그 자리에서 입력이 막힌다.
          this.pipelines.get(ref.viewId)?.handleExit(event.payload);
          this.patternCooldown.clear(event.payload.session_id);
          this.patternTails.delete(event.payload.session_id);
          // 에이전트 마킹은 종료 처리로 지우기 전에 읽는다 — 이어서 열 대화가
          // 있을 수 있는 창인지 가르는 근거다(자동 닫기 판단).
          const before = useWorkbenchStore.getState().panes[ref.leafId];
          const mayResume = Boolean(before?.resume ?? before?.agent);
          // 종료 사유 한 줄(W1-1): 이벤트 페이로드의 reason/detail을
          // pane이 스스로 기억해 exited 오버레이가 근거를 보여 준다.
          const exit = {
            code: event.payload.exit_code,
            reason: event.payload.reason,
            detail: event.payload.detail ?? null,
          };
          // 정상 종료(04-ui §5-1): 이어서 열 대화가 없는 일반 터미널은 종료
          // 표시를 거치지 않고 바로 닫는다 — 오버레이가 한 프레임도 비치지
          // 않게. 에이전트 마킹이 있으면 기록 조회가 빈손이었을 때만 이어간다.
          const closeOnExit = isCleanExit(exit) && !this.isManagedPane(before ?? null);
          if (closeOnExit && !mayResume) {
            void this.autoCloseExitedPane(ref.leafId);
            break;
          }
          store.panePhase(ref.leafId, "exited");
          store.paneInterventionBadge(ref.leafId, false);
          // 세션이 끝났으면 감지 배지도 내린다(요약의 agent는 잔상이다).
          store.paneAgent(ref.leafId, null);
          store.paneExit(ref.leafId, exit);
          void this.updateExitedPaneResume(ref.leafId, closeOnExit);
        }
        break;
      }
      case "session.owner_changed": {
        // R1 단일 writer UI는 take_control을 노출하지 않는다(계약만 유지).
        break;
      }
      case "session.replay_required": {
        // 데몬이 이 view를 떼어냈다(보존 창보다 뒤처짐·데이터 연결 소실·원장
        // 어긋남) — 그 pane만 새 epoch로 다시 붙인다(02-runner §5). 잇단
        // 알림은 backoff로 합쳐 무한 reset+재생 폭주(깜빡임·입력 차단)를 막는다.
        const ref = this.sessionIndex.get(event.payload.session_id);
        if (ref && ref.viewId === event.payload.view_id) {
          this.scheduleReattach(ref, event.payload.session_id);
        }
        break;
      }
      case "workload.changed": {
        store.upsertWorkload(event.payload);
        this.maybeNotifyFinished(event.payload);
        if (!isFinishedWorkload(event.payload.state) && (event.payload.usage?.cpu_cores.value ?? 0) >= 0.02) {
          for (const pane of Object.values(useWorkbenchStore.getState().panes)) {
            if (pane.workloadId === event.payload.workload_id && pane.sessionId && pane.phase === "live") markTerminalActivity(pane.sessionId);
          }
        }
        this.refreshPaneUsages();
        this.refreshPaneAgents();
        this.refreshPaneRelief();
        break;
      }
      case "intervention.reported": {
        // W1-5: 개입 신호 → 알림센터. 조용한 우선(§2.1): 항목은 항상,
        // 데스크톱 알림은 "창이 숨겨져 있을 때 + 승인/질문"만.
        const notice = event.payload;
        const sessionId = this.matchSessionByHint(notice.session_hint ?? null);
        const added = useNotificationStore.getState().push({
          id: notice.report_id,
          kind: "intervention",
          title: notice.title,
          detail: notice.detail ?? null,
          at: notice.reported_at,
          sessionId,
          interventionKind: notice.kind,
          acknowledged: false,
        });
        if (
          added &&
          (notice.kind === "permission" || notice.kind === "question")
        ) {
          maybeDesktopNotify(
            t(interventionKindKey(notice.kind)),
            notice.title,
          );
        }
        break;
      }
      case "queue.changed":
        store.setQueue(event.payload.queue);
        break;
      case "resource.snapshot": {
        store.setHostSample(event.payload);
        resourceHistory.push(event.payload);
        this.refreshPaneUsages();
        this.refreshPaneRelief();
        break;
      }
      default:
        break;
    }
  }

  /** session_hint(cwd 또는 세션 id)로 pane을 찾아 이동 대상을 만든다. */
  private matchSessionByHint(hint: string | null): string | null {
    if (!hint) return null;
    const panes = Object.values(useWorkbenchStore.getState().panes);
    const exact = panes.find((pane) => pane.sessionId === hint);
    if (exact) return exact.sessionId;
    const byCwd = panes.find((pane) => pane.cwd === hint);
    return byCwd?.sessionId ?? null;
  }

  /**
   * 종료 알림(W1-3 조용한 신호의 확장): 관리 작업이 끝났는데 그 pane이
   * 활성 탭에 없으면 알림센터에 한 줄 남긴다. 활성 탭이면 사용자가 보고
   * 있으니 침묵한다.
   */
  private maybeNotifyFinished(workload: {
    workload_id: string;
    state: string;
    title: string;
    mode?: string;
  }): void {
    // 셸(mode="shell") 종료는 일상 동작이다(exit 입력) — 알림센터를 채우지
    // 않는다. 관리 작업(에이전트)의 종료만 알린다(§2.1 조용한 우선).
    if (workload.mode === "shell") return;
    if (!isFinishedWorkload(workload.state as never)) return;
    const state = useWorkbenchStore.getState();
    const pane = Object.values(state.panes).find((p) => p.workloadId === workload.workload_id);
    if (!pane) return; // 화면에 없는 작업은 대기열 drawer의 영역이다
    const tab = state.tabs.find((t) => t.id === state.activeTabId);
    const inActiveTab =
      tab?.kind === "terminal" && tab.root ? listLeaves(tab.root).some((leaf) => leaf.id === pane.leafId) : false;
    if (inActiveTab) return;
    useNotificationStore.getState().push({
      id: `finished:${workload.workload_id}:${workload.state}`,
      kind: "workload-finished",
      title: `${terminalDisplayTitle(workload.title)} — ${workloadStateText(workload.state as never)}`,
      detail: pane.exit ? null : null,
      at: new Date().toISOString(),
      sessionId: pane.sessionId,
      acknowledged: false,
    });
  }

  /**
   * 저신뢰 패턴 감지(W3-5): opt-in 배지 전용. live 출력만 오고(재생
   * 제외), 쿨다운으로 재발화를 막는다. 데스크톱 알림과 결합하지 않는다.
   */
  private handleOutputText(leafId: string, sessionId: string, text: string): void {
    if (!usePreferences.getState().interventionTextBadge) return;
    // 문구가 출력 청크 경계를 가로지를 수 있어 직전 꼬리와 이어서 본다.
    const tail = this.patternTails.get(sessionId) ?? "";
    const combined = tail + text;
    this.patternTails.set(
      sessionId,
      combined.length > 512 ? combined.slice(combined.length - 512) : combined,
    );
    const pattern = detectInterventionPattern(combined);
    if (!pattern) return;
    if (!this.patternCooldown.shouldReport(sessionId, pattern, Date.now())) return;
    useWorkbenchStore.getState().paneInterventionBadge(leafId, true);
  }

  /** 사용자가 pane을 보면 배지를 끈다(조용한 신호의 확인 동작). */
  clearInterventionBadge(leafId: string): void {
    useWorkbenchStore.getState().paneInterventionBadge(leafId, false);
  }

  private refreshPaneUsages(): void {
    const store = useWorkbenchStore.getState();
    const byWorkload = new Map(store.workloads.map((w) => [w.workload_id, w]));
    for (const pane of Object.values(store.panes)) {
      if (!pane.workloadId) continue;
      const workload = byWorkload.get(pane.workloadId);
      const usage = workload?.usage ?? null;
      const next = usage
        ? {
            cpuCores: usage.cpu_cores.value,
            residentBytes: parseU64(usage.resident_bytes.value),
          }
        : null;
      const current = pane.usage;
      if (
        (next === null) !== (current === null) ||
        (next && current && (next.cpuCores !== current.cpuCores || next.residentBytes !== current.residentBytes))
      ) {
        store.paneUsage(pane.leafId, next);
      }
    }
  }

  /**
   * 완화 상태 거울(08-pressure-relief §2): 데몬 요약의 relief/protected를
   * pane으로 내린다. usage와 같은 자리에서 돌고, 값이 그대로면 쓰지 않는다 —
   * 완화가 바뀌는 순간에만 헤더가 다시 그려진다.
   */
  private refreshPaneRelief(): void {
    const store = useWorkbenchStore.getState();
    const byWorkload = new Map(store.workloads.map((w) => [w.workload_id, w]));
    for (const pane of Object.values(store.panes)) {
      if (!pane.workloadId) continue;
      const workload = byWorkload.get(pane.workloadId);
      // 요약에서 사라진 작업은 완화를 논할 대상이 아니다(프로세스가 없다).
      const relief = paneReliefFrom(workload?.relief);
      const isProtected = workload?.protected ?? false;
      const current = pane.relief ?? PANE_RELIEF_NONE;
      if (!sameRelief(current, relief) || (pane.protected ?? false) !== isProtected) {
        store.paneRelief(pane.leafId, relief, isProtected);
      }
    }
  }

  /**
   * 자동 감지된 에이전트(에이전트 감시 루프 → WorkloadSummary.agent)를
   * pane으로 내린다. 종료한 세션은 요약이 잔상을 남겨도 배지를 유지하지
   * 않는다(session.exited에서 이미 지웠다).
   */
  private refreshPaneAgents(): void {
    const store = useWorkbenchStore.getState();
    const byWorkload = new Map(store.workloads.map((w) => [w.workload_id, w]));
    for (const pane of Object.values(store.panes)) {
      if (!pane.workloadId) continue;
      if (pane.phase === "exited" || pane.phase === "failed") continue;
      const next = byWorkload.get(pane.workloadId)?.agent ?? null;
      // 값이 같으면 건드리지 않는다 — 참조 비교였을 때는 workload.changed가
      // 올 때마다 모든 pane을 갈아 끼워 header 전부가 다시 그려졌다.
      if (!sameAgentStatus(pane.agent ?? null, next)) store.paneAgent(pane.leafId, next);
      // 배경 tint는 변화 여부와 무관하게 항상 최신으로 맞춘다 — 뷰가 이후에
      // 만들어지는 경우(앱 복원·미뤄진 attach) registry가 저장된 값을 적용한다.
      this.applyPaneBackground(pane.leafId);
    }
  }

  // ------------------------------------------------------------ pane 배경색

  /** tint 혼합의 베이스가 될 현재 테마 배경(전역 적용 전이면 설정에서 계산). */
  private terminalBackgroundBase(): string {
    const applied = this.registry.themeBackground();
    if (applied) return applied;
    return terminalTheme(resolveTheme(usePreferences.getState().theme, systemPrefersLight()))
      .background;
  }

  /**
   * 이 pane의 배경색(사용자 지정 > 에이전트 tint > 테마 기본)을 실제
   * 터미널에 적용한다. pane이 없으면 아무것도 하지 않는다.
   */
  applyPaneBackground(leafId: string): void {
    const pane = useWorkbenchStore.getState().panes[leafId];
    if (!pane) return;
    const color = paneBackgroundColor(
      usePreferences.getState(),
      { custom: pane.backgroundColor ?? null, agent: pane.agent ?? null },
      this.terminalBackgroundBase(),
    );
    this.registry.setBackgroundOverride(pane.viewId, color);
  }

  /** 모든 pane의 배경색을 다시 계산해 적용한다(테마·설정 변경 때). */
  applyAllPaneBackgrounds(): void {
    for (const leafId of Object.keys(useWorkbenchStore.getState().panes)) {
      this.applyPaneBackground(leafId);
    }
  }

  /** 사용자가 pane 배경색을 직접 고른다(null이면 지정 해제 — tint/테마로). */
  setPaneBackgroundColor(leafId: string, color: string | null): void {
    useWorkbenchStore.getState().paneBackgroundColor(leafId, color);
    this.applyPaneBackground(leafId);
  }

  // ------------------------------------------------------------ pane create

  /**
   * 새 터미널 버튼: 빈 탭이면 첫 pane, 아니면 focused pane을 좌우 분할.
   *
   * 반환값은 "pane을 만들었는가"다 — pane/세션 한도·공간 부족으로 거절되면
   * false이고, 그 사유는 splitFocused가 이미 문구로 알린다.
   */
  newTerminal(shell?: ShellSpec): boolean {
    if (shell) this.nextShell = shell;
    const store = useWorkbenchStore.getState();
    const tab = store.tabs.find((t) => t.id === store.activeTabId) ?? null;
    // mission 탭에는 pane을 만들지 않는다(05 §2) — 새 terminal 탭을 연다.
    if (!tab || tab.kind !== "terminal" || !tab.root) {
      const tabId = tab?.kind === "terminal" ? tab.id : this.createTab();
      return this.createFirstPane(tabId);
    }
    return this.splitFocused("row");
  }

  /** 새 탭(팔레트/tab bar +). */
  newTab(): string {
    return this.createTab();
  }

  /**
   * 배치 편집의 "+ 새 그룹": 빈 terminal 탭을 맨 끝에 만들되 보고 있는 탭은 바꾸지 않는다 —
   * 정리하는 중에 돌아갈 자리가 빈 탭으로 바뀌면 안 된다(탭이 하나도 없을 때만 그 탭이 활성).
   */
  newGroup(): string {
    const tabId = this.uuid();
    const store = useWorkbenchStore.getState();
    store.addTab(tabId, t("app.tabTitle", { index: store.tabs.length + 1 }));
    return tabId;
  }

  // ------------------------------------------------- 재배치(04-ui §2-5)
  // 아래 넷은 트리만 옮긴다: 세션·PTY·xterm·pane 메타는 그대로이고, 화면은
  // 탭 전환과 같은 unmount → mount → refit 경로를 탄다. 데몬 호출은 없다.

  /** pane을 다른 탭으로(대상 탭의 초점 pane 옆에 좌우 분할). 상한(8)은 분할과 같은 문구로 거절. */
  movePaneToTab(leafId: string, targetTabId: string): boolean {
    const store = useWorkbenchStore.getState();
    const target = store.tabs.find((tab) => tab.id === targetTabId);
    // mission/agent-view 탭은 pane을 받지 않는다(05 §2) — 대상 목록에도 없다.
    if (!target || target.kind !== "terminal" || !store.panes[leafId]) return false;
    if (!canAddPane(target.root)) {
      this.toast(paneLimitText(MAX_PANES_PER_TAB));
      return false;
    }
    const ok = store.movePaneToTab(leafId, targetTabId, this.uuid());
    if (!ok) this.toast(t("terminal.move.failed"));
    return ok;
  }

  /**
   * pane을 새 탭으로 떼어 낸다. 새 탭 이름은 그 pane의 프로젝트 키(cwd —
   * pwd)의 마지막 조각 — 재그룹핑이 짓는 이름과 같은 규칙이다. 혼자 있는
   * pane은 이미 탭이므로 문구만 띄운다. atIndex를 주면(끌어 놓기) 그 틈에 새 탭을 둔다.
   */
  detachPaneToNewTab(leafId: string, atIndex?: number): boolean {
    const store = useWorkbenchStore.getState();
    const pane = store.panes[leafId];
    if (!pane) return false;
    const tab = store.tabs.find((candidate) => candidate.kind === "terminal" && findLeaf(candidate.root, leafId) !== null);
    if (!tab || tab.kind !== "terminal" || leafCount(tab.root) <= 1) {
      this.toast(t("terminal.move.alone"));
      return false;
    }
    const key = paneProjectKey(pane);
    const title = key ? projectTitle(key) : t("app.tabTitle", { index: store.tabs.length + 1 });
    const ok = store.detachPaneToNewTab(leafId, this.uuid(), title, atIndex);
    if (!ok) this.toast(t("terminal.move.failed"));
    return ok;
  }

  /** source 탭의 pane 전부를 target 탭에 합친다(두 트리를 좌우 split 하나로 잇는다). */
  mergeTabs(sourceTabId: string, targetTabId: string): boolean {
    const store = useWorkbenchStore.getState();
    const source = store.tabs.find((tab) => tab.id === sourceTabId);
    const target = store.tabs.find((tab) => tab.id === targetTabId);
    if (!source || !target || source.id === target.id) return false;
    // 합치기는 terminal 탭끼리만 — mission/agent-view 탭에는 합칠 트리가 없다(05 §2).
    if (source.kind !== "terminal" || target.kind !== "terminal") return false;
    if (leafCount(source.root) + leafCount(target.root) > MAX_PANES_PER_TAB) {
      this.toast(paneLimitText(MAX_PANES_PER_TAB));
      return false;
    }
    const ok = store.mergeTabs(sourceTabId, targetTabId, this.uuid());
    if (!ok) this.toast(t("terminal.move.failed"));
    return ok;
  }

  /** 탭을 한 칸 옮긴다(delta ±1). 끝에서는 아무 일도 없다. */
  moveTab(tabId: string, delta: number): void {
    const store = useWorkbenchStore.getState();
    const index = store.tabs.findIndex((tab) => tab.id === tabId);
    if (index < 0) return;
    store.moveTab(tabId, index + delta);
  }

  /** 탭을 지정한 자리로 옮긴다(끌어 놓기 — 인덱스는 store가 범위로 자른다). */
  placeTab(tabId: string, toIndex: number): void {
    useWorkbenchStore.getState().moveTab(tabId, toIndex);
  }

  /**
   * pane을 다른 pane의 가장자리 옆에 놓는다(끌어 놓기). 다른 탭으로 옮길 때만 상한(8)을
   * 분할과 같은 문구로 거절한다 — 같은 탭 안의 재배치는 창 수가 그대로다.
   */
  dockPane(leafId: string, targetLeafId: string, edge: PaneEdge): boolean {
    const store = useWorkbenchStore.getState();
    const holding = (id: string) =>
      store.tabs.find((tab) => tab.kind === "terminal" && findLeaf(tab.root, id) !== null) ?? null;
    const source = holding(leafId);
    const target = holding(targetLeafId);
    if (!source || !target || target.kind !== "terminal" || !store.panes[leafId]) return false;
    if (source.id !== target.id && !canAddPane(target.root)) {
      this.toast(paneLimitText(MAX_PANES_PER_TAB));
      return false;
    }
    const ok = store.dockPane(leafId, targetLeafId, edge, this.uuid());
    if (!ok) this.toast(t("terminal.move.failed"));
    return ok;
  }

  /** 두 pane의 자리를 맞바꾼다(끌어 놓기 — 같은 탭·다른 탭). 창 수가 그대로라 상한과 무관하다. */
  swapPanes(leafId: string, targetLeafId: string): boolean {
    const ok = useWorkbenchStore.getState().swapPanes(leafId, targetLeafId);
    if (!ok) this.toast(t("terminal.move.failed"));
    return ok;
  }

  /**
   * 끌어 놓기로 고른 동작(layoutDrag.resolveLayoutDrop)을 재배치 경로로 보낸다. 메뉴·
   * 팔레트와 같은 메서드를 거치므로 상한·실패 문구도 같다.
   */
  applyLayoutDrop(action: LayoutDropAction): boolean {
    // 지우는 중인 그룹(detach 왕복 대기)에는 넣지도 빼지도 않는다 — 곧 사라질 탭이다.
    if (this.touchesUngroupingTab(action)) {
      this.toast(t("terminal.move.failed"));
      return false;
    }
    switch (action.kind) {
      case "reorder-tab": {
        this.placeTab(action.tabId, action.toIndex);
        // 혼자인 창의 헤더로 끈 경우는 다른 창 동작처럼 그 창을 따라간다 — 끌며 머물러 다른 탭을
        // 열어 두었어도 옮긴 탭이 보이고, 입력이 엉뚱한 셸로 가지 않는다.
        if (action.focusLeafId) {
          const store = useWorkbenchStore.getState();
          store.setActiveTab(action.tabId);
          store.focusPane(action.focusLeafId);
        }
        return true;
      }
      case "merge-tab":
        return this.mergeTabs(action.sourceTabId, action.targetTabId);
      case "move-pane-to-tab":
        return this.movePaneToTab(action.leafId, action.targetTabId);
      case "detach-pane":
        return this.detachPaneToNewTab(action.leafId, action.atIndex ?? undefined);
      case "dock-pane":
        return this.dockPane(action.leafId, action.targetLeafId, action.edge);
      case "swap-panes":
        return this.swapPanes(action.leafId, action.targetLeafId);
      case "attach-session":
        return this.attachSessionAt(action.sessionId, action.workloadId, action.placement);
    }
  }

  /** 레이아웃 드래그 프리뷰(임계값 통과 알림) — LayoutDragLayer가 그린다. */
  layoutDragPreview(_target: { kind: "tab" | "pane"; tabId?: string; leafId?: string }): void {
    void _target;
  }

  /** 레이아웃 드래그 종료 — 실제 재배치는 applyLayoutDrop 경로로 커밋된다. */
  layoutDragEnd(_target: { kind: "tab" | "pane"; tabId?: string; leafId?: string }): void {
    void _target;
  }

  /** 그 끌어 놓기가 지우는 중인 그룹(탭)에서 창을 빼거나 그 그룹에 넣는가. */
  private touchesUngroupingTab(action: LayoutDropAction): boolean {
    if (this.ungroupingTabs.size === 0) return false;
    const tabs = useWorkbenchStore.getState().tabs;
    const holding = (leafId: string) =>
      tabs.find((tab) => tab.kind === "terminal" && findLeaf(tab.root, leafId) !== null)?.id ?? null;
    const involved: Array<string | null> = [];
    switch (action.kind) {
      case "reorder-tab":
        involved.push(action.tabId);
        break;
      case "merge-tab":
        involved.push(action.sourceTabId, action.targetTabId);
        break;
      case "move-pane-to-tab":
        involved.push(holding(action.leafId), action.targetTabId);
        break;
      case "detach-pane":
        involved.push(holding(action.leafId));
        break;
      case "dock-pane":
      case "swap-panes":
        involved.push(holding(action.leafId), holding(action.targetLeafId));
        break;
      case "attach-session":
        if (action.placement.kind === "tab") involved.push(action.placement.tabId);
        else if (action.placement.kind === "beside") involved.push(holding(action.placement.leafId));
        break;
    }
    return involved.some((tabId) => tabId !== null && this.ungroupingTabs.has(tabId));
  }

  /**
   * 배치 편집: 어느 pane에도 붙어 있지 않은 실행 중 세션(창만 닫기·그룹 삭제로 떨어져 나간
   * 터미널)을 지정한 자리에 새 pane으로 붙인다. 세션당 화면은 하나라 이미 붙어 있으면 거절한다
   * (그런 터미널은 목록에서 pane으로 끌려 옮겨진다). 창이 하나 늘어나므로 상한(8)을 본다.
   */
  attachSessionAt(sessionId: string, workloadId: string | null, placement: LayoutPlacement): boolean {
    // 세션당 화면은 하나다: pane 메타뿐 아니라 살아 있는 view 경로(sessionIndex)도 본다. 가리키는 pane이
    // 이미 스토어에 없는 경로는 낡은 것이라 먼저 치운다 — 남겨 두면 그 세션을 영영 다시 붙일 수 없다.
    this.dropStaleSessionRoute(sessionId);
    const store = useWorkbenchStore.getState();
    if (this.sessionIndex.has(sessionId) || Object.values(store.panes).some((pane) => pane.sessionId === sessionId)) {
      this.toast(t("terminal.move.failed"));
      return false;
    }
    if (placement.kind !== "new-tab") {
      const target =
        placement.kind === "tab"
          ? store.tabs.find((tab) => tab.id === placement.tabId)
          : store.tabs.find((tab) => tab.kind === "terminal" && findLeaf(tab.root, placement.leafId) !== null);
      if (target?.kind === "terminal" && !canAddPane(target.root)) {
        this.toast(paneLimitText(MAX_PANES_PER_TAB));
        return false;
      }
    }
    const workload =
      store.workloads.find((candidate) => candidate.session_id === sessionId) ??
      (workloadId ? store.workloads.find((candidate) => candidate.workload_id === workloadId) : undefined);
    const resolvedWorkloadId = workload?.workload_id ?? workloadId;
    const cwd = workload?.cwd ?? null;
    const leafId = this.uuid();
    const placedTab = store.placeNewPane(
      placement,
      {
        leafId,
        viewId: this.uuid(),
        sessionId,
        workloadId: resolvedWorkloadId,
        title: workload?.title ?? t("terminal.reattach"),
        cwd,
        phase: "replaying",
      },
      {
        splitId: this.uuid(),
        tabId: this.uuid(),
        // 새 그룹 이름은 새 탭으로 분리와 같은 규칙: 그 터미널 경로의 마지막 조각.
        tabTitle: cwd ? projectTitle(cwd) : t("app.tabTitle", { index: store.tabs.length + 1 }),
      },
    );
    if (!placedTab) {
      this.toast(t("terminal.move.failed"));
      return false;
    }
    void this.attachPane(leafId, sessionId, resolvedWorkloadId);
    return true;
  }

  /**
   * 배치 편집의 그룹 삭제: 탭을 닫되 터미널은 끄지 않는다(창만 닫기) — 세션은 계속 실행되고 배치
   * 안 됨 목록으로 옮겨 가 다시 끌어 넣을 수 있다. mission 계열 탭은 대상이 아니다.
   */
  async ungroupTab(tabId: string): Promise<void> {
    const initial = useWorkbenchStore.getState().tabs.find((candidate) => candidate.id === tabId);
    // 같은 그룹을 두 번 지우지 않는다(detach 왕복을 기다리는 사이 ×를 또 눌러도).
    if (!initial || initial.kind !== "terminal" || this.ungroupingTabs.has(tabId)) return;
    this.ungroupingTabs.add(tabId);
    let keptRunning = 0;
    try {
      // detach를 기다리는 사이 이 그룹에 창이 들어올 수 있다(메뉴 경로 등) — 빌 때까지 지금 들어 있는
      // 창을 다시 읽어 창만 닫기로 뗀다. 탭 닫기가 떼지 않은 창을 스토어에서만 지우면 그 view·pipeline이
      // 고아로 남는다. 마지막 확인과 탭 닫기 사이에는 await가 없어 끼어들 틈이 없다.
      const attempted = new Set<string>();
      for (;;) {
        const tab = useWorkbenchStore.getState().tabs.find((candidate) => candidate.id === tabId);
        const pending = tab?.kind === "terminal" ? listLeaves(tab.root).map((leaf) => leaf.id).filter((id) => !attempted.has(id)) : [];
        if (pending.length === 0) break;
        for (const leafId of pending) {
          attempted.add(leafId);
          const now = useWorkbenchStore.getState();
          const current = now.tabs.find((candidate) => candidate.id === tabId);
          // 기다리는 사이 다른 그룹으로 옮겨진 창은 더 이상 이 그룹의 것이 아니다.
          if (current?.kind !== "terminal" || !findLeaf(current.root, leafId)) continue;
          const pane = now.panes[leafId];
          if (pane && this.keepsRunningAfterDetach(pane)) keptRunning += 1;
          await this.closePane(leafId, false);
        }
      }
      useWorkbenchStore.getState().closeTab(tabId);
    } finally {
      this.ungroupingTabs.delete(tabId);
    }
    if (keptRunning > 0) this.toast(t("layoutEditor.groupDeleted", { n: keptRunning }));
  }

  /** 지우는 중인 그룹(탭) — 그 사이 끌어 놓기로 넣거나 빼지 않는다. */
  private readonly ungroupingTabs = new Set<string>();

  /**
   * 창만 닫기로 떼어도 계속 도는 터미널인가: 살아 있는 세션이거나, 아직 세션 없이 시작 중인 실행
   * (시작이 끝나면 떨어져 나간 채 배치 안 됨에 나타난다).
   */
  private keepsRunningAfterDetach(pane: PaneMeta): boolean {
    if (pane.sessionId) return pane.phase !== "exited" && pane.phase !== "failed";
    return pane.phase === "starting";
  }

  /**
   * 스토어에서 이미 사라진 pane을 가리키는 세션 경로를 치운다(작업 공간을 컨트롤러 밖에서 갈아 끼운
   * 경우 등). 그 view의 pipeline·출력 채널·xterm을 버리고 데몬에서도 떼어, 새로 붙는 화면과 한 세션을
   * 두고 다투지 않게 한다.
   */
  private dropStaleSessionRoute(sessionId: string): void {
    const route = this.sessionIndex.get(sessionId);
    if (!route || useWorkbenchStore.getState().panes[route.leafId]) return;
    this.sessionIndex.delete(sessionId);
    this.pipelines.get(route.viewId)?.dispose();
    this.pipelines.delete(route.viewId);
    this.releaseSessionRoute(sessionId, route.viewId);
    this.disposeView(route.viewId);
    void this.deps.client.sessionDetach({ session_id: sessionId, view_id: route.viewId }).catch(() => undefined);
  }

  /**
   * 프로젝트별로 다시 묶기: 계획을 세워 미리보기 대화상자를 연다. 바뀔 것이
   * 없으면(이미 프로젝트별) 문구만 띄운다. 적용은 사용자가 확인한 뒤
   * `applyRegroup`에서 한다.
   */
  regroupByProject(): void {
    const store = useWorkbenchStore.getState();
    const plan = planRegroup(
      { tabs: store.tabs, panes: store.panes, activeTabId: store.activeTabId, focusedLeafId: store.focusedLeafId },
      { noProjectTitle: t("regroup.noProject") },
    );
    if (!plan.changed) {
      this.toast(t("regroup.noChange"));
      return;
    }
    store.openModal({ kind: "regroup", plan });
  }

  /** 미리보기에서 확인한 계획을 적용한다. 그사이 배치가 바뀌었으면 아무것도 바꾸지 않는다. */
  applyRegroup(plan: RegroupPlan): boolean {
    const ok = useWorkbenchStore.getState().applyRegroup(plan, () => this.uuid());
    this.toast(ok ? t("regroup.applied", { n: useWorkbenchStore.getState().tabs.length }) : t("regroup.failed"));
    return ok;
  }

  private createTab(): string {
    const tabId = this.uuid();
    useWorkbenchStore.getState().addTab(tabId, t("app.tabTitle", { index: useWorkbenchStore.getState().tabs.length + 1 }));
    useWorkbenchStore.getState().setActiveTab(tabId);
    return tabId;
  }

  /**
   * cwd를 주면 그 경로에서 시작한다(에이전트 세션 재개 — 04-ui §5).
   * 빈 탭의 첫 pane에는 한도 판정이 없어 항상 만든다 — 분할 경로와 반환값을
   * 맞추기 위해 true를 돌려준다.
   */
  createFirstPane(tabId: string, cwd?: string): boolean {
    const leafId = this.uuid();
    const viewId = this.uuid();
    const root = makeLeaf(leafId, viewId, null);
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((t) => (t.id === tabId && t.kind === "terminal" ? { ...t, root } : t)),
      panes: {
        ...s.panes,
        [leafId]: {
          leafId,
          viewId,
          sessionId: null,
          workloadId: null,
          title: t("terminal.newTerminal"),
          cwd: null,
          phase: "starting",
          error: null,
          usage: null,
          flowBlocked: false,
          relief: PANE_RELIEF_NONE,
          protected: false,
        },
      },
      focusedLeafId: leafId,
    }));
    void this.launchShell(leafId, cwd ?? this.pickCwd(null));
    return true;
  }

  /** 시작 cwd: 신뢰 cwd → 프로젝트 root → 사용자 home (04-ui §2-4). */
  private pickCwd(trustedCwd: string | null): string {
    // 저장된 레이아웃의 옛 값·WSL의 POSIX 경로 등 이 플랫폼에서 못 쓰는
    // cwd는 버리고 다음 후보로 간다(데몬이 거절해 pane이 실패하는 대신).
    const usable = trustedCwd ? acceptReportedCwd(trustedCwd, this.deps.platform) : null;
    return usable ?? this.config.projectRoot ?? this.config.home;
  }

  /**
   * 시작 cwd 후보를 앞선 순서대로(중복 없이): 이 플랫폼에서 쓸 수 있는 선호 경로마다
   * 그 경로 → 가까운 상위 경로들, 그다음 프로젝트 root(→ 상위) → 사용자 home →
   * 파일시스템 뿌리. 첫 값은 `pickCwd(preferred[0])`와 같다 — 나머지는 그 경로를
   * 쓸 수 없을 때(CWD_UNAVAILABLE) 이어서 시도한다. 뿌리는 늘 있으므로 마지막 자리다.
   */
  private cwdCandidates(...preferred: Array<string | null | undefined>): string[] {
    const platform = this.deps.platform;
    const candidates: string[] = [];
    const add = (cwd: string | null | undefined): void => {
      if (cwd && !candidates.includes(cwd)) candidates.push(cwd);
    };
    const accepted = preferred.map(cwd => (cwd ? acceptReportedCwd(cwd, platform) : null));
    for (const cwd of [...accepted, this.config.projectRoot]) {
      if (!cwd) continue;
      add(cwd);
      ancestorDirs(cwd, platform).forEach(add);
    }
    add(this.config.home);
    add(filesystemRoot(this.config.home, platform));
    return candidates;
  }

  /** 이 view xterm의 지금 격자(데몬 허용 범위일 때만). */
  /**
   * 새 pane의 첫 PTY 크기: 터미널이 mount되어 host를 잴 수 있으면 그 격자
   * (registry가 붙기 전 fit에서 xterm에도 곧바로 맞춘다), 아니면 프레임을
   * 몇 번 기다린 뒤 다시 본다. rAF가 없는 환경(node 시험)이나 끝내 재지
   * 못하면 터미널의 현재 격자(없으면 null → 80×24, 붙은 뒤 fit이 맞춘다).
   */
  private async measuredGrid(viewId: string): Promise<{ cols: number; rows: number } | null> {
    const raf = typeof requestAnimationFrame === "function" ? requestAnimationFrame : null;
    for (let frame = 0; ; frame += 1) {
      const entry = this.deps.registry.get(viewId);
      const measured = typeof entry?.measure === "function" ? entry.measure() : null;
      if (measured) return measured;
      if (!raf || frame >= LAUNCH_MEASURE_FRAMES || this.disposed) return this.terminalGrid(viewId);
      await new Promise<void>((resolve) => raf(() => resolve()));
    }
  }

  private terminalGrid(viewId: string): { cols: number; rows: number } | null {
    const terminal = this.deps.registry.get(viewId)?.terminal as { cols?: number; rows?: number } | undefined;
    const fits = (n: number | undefined): n is number => Number.isInteger(n) && n! >= 2 && n! <= 1000;
    return terminal && fits(terminal.cols) && fits(terminal.rows) ? { cols: terminal.cols, rows: terminal.rows } : null;
  }

  /** 새 작업의 기본 cwd(퀵스타트 카드 등 UI가 묻는 용도). */
  defaultCwd(): string {
    return this.pickCwd(null);
  }

  /**
   * cwd를 주면 새 pane은 그 경로에서 시작한다(에이전트 세션 재개).
   * 반환값은 "pane을 만들었는가" — 거절 사유는 한도·공간 문구로 알린다.
   */
  splitFocused(axis: "row" | "column", cwdOverride?: string): boolean {
    const store = useWorkbenchStore.getState();
    const tab = store.tabs.find((t) => t.id === store.activeTabId);
    // mission 탭 활성 중 분할은 새 terminal 탭의 첫 pane이 된다(05 §2).
    if (!tab || tab.kind !== "terminal" || !tab.root) {
      if (cwdOverride === undefined) return this.newTerminal();
      return this.createFirstPane(tab?.kind === "terminal" ? tab.id : this.createTab(), cwdOverride);
    }
    const focusedLeafId = store.focusedLeafId && store.panes[store.focusedLeafId] ? store.focusedLeafId : null;
    if (!focusedLeafId) return false;
    if (!canAddPane(tab.root)) {
      this.toast(paneLimitText(MAX_PANES_PER_TAB));
      return false;
    }
    if (this.liveSessions.size >= SESSIONS_LIMIT) {
      this.toast(t("terminal.session.limit", { count: SESSIONS_LIMIT }));
      return false;
    }
    const size = this.paneSizes.get(store.panes[focusedLeafId].viewId) ?? { width: 1e6, height: 1e6 };
    const check = canSplit(size, axis);
    if (!check.ok) {
      this.toast(splitNoSpaceText());
      return false;
    }
    const leafId = this.uuid();
    const viewId = this.uuid();
    const cwd = cwdOverride ?? this.pickCwd(store.panes[focusedLeafId].cwd);
    const ok = useWorkbenchStore.getState().applySplit(
      tab.id,
      focusedLeafId,
      { leafId, viewId, sessionId: null, title: t("terminal.newTerminal"), cwd },
      this.uuid(),
      axis,
    );
    if (!ok) return false;
    void this.launchShell(leafId, cwd);
    return true;
  }

  // ------------------------------------------------------------- launching

  /**
   * 일반 셸·셸 프로필은 열리는 쪽으로 버틴다(04-ui §2-4):
   *  - 시작 경로를 쓸 수 없으면(CWD_UNAVAILABLE — 지운 디렉터리, 빠진 디스크, 권한 없음)
   *    가까운 상위 경로 → 프로젝트 root → home → 뿌리 순으로 연다. `fallbackCwds`를
   *    주면 그 목록을 쓴다(끝난 창을 이을 때 처음 실행한 경로를 끼워 넣는다).
   *  - 셸 실행 파일이 없거나(PROGRAM_NOT_FOUND) PTY를 띄우지 못하면(SPAWN_FAILED)
   *    플랫폼 기본 셸 → /bin/sh로 바꿔 연다.
   * 바꿔 열었으면 무엇을 바꿨는지 알린다. 에이전트 대화 재개(`agent`가 붙은 셸)는 대화가
   * 그 경로·그 CLI에 묶여 있어 바꿔 열지 않고, 사유를 남겨 "새 셸"로 잇게 한다.
   * `grid`: 이미 그려진 화면에서 잇는 PTY의 첫 크기(없으면 80×24 뒤 fit이 맞춘다).
   */
  private async launchShell(
    leafId: string,
    cwd: string,
    options: { fallbackCwds?: readonly string[]; grid?: { cols: number; rows: number } | null } = {},
  ): Promise<void> {
    const store = useWorkbenchStore.getState();
    const pane = store.panes[leafId];
    if (!pane) return;
    const shell: ShellSpec = this.nextShell ?? this.deps.resolveShell?.() ?? {
      program: defaultShellProgram(this.deps.platform),
      argv: defaultShellArgv(this.deps.platform),
      label: t("terminal.fallbackShell"),
    };
    this.nextShell = null; // 1회용: 다음 생성은 다시 기본 프로필
    const resuming = shell.agent !== undefined;
    const fallbackCwds = resuming ? [] : options.fallbackCwds ?? this.cwdCandidates(cwd).filter(c => c !== cwd);
    const shells = resuming ? [shell] : this.shellCandidates(shell);
    // 새 pane의 PTY는 처음부터 창 크기로 연다: pane이 mount되어 격자를 재기까지
    // 한두 프레임 기다린다 — 80×24로 열렸다가 붙은 뒤 fit으로 펴지는 왕복을 없앤다.
    const grid = options.grid ?? (await this.measuredGrid(pane.viewId));
    const request: LaunchRequest = {
      request_id: this.uuid(),
      profile_id: "shell",
      cwd,
      ...cleanShellCommand(this.deps.platform, shell),
      env_overrides: {},
      mode: "shell",
      executor: { kind: "local" },
      cols: grid?.cols ?? 80,
      rows: grid?.rows ?? 24,
      priority: 1,
      policy: defaultShellPolicy(),
    };
    // Claude 재개만 제공자 라우팅 대상이다(일반 셸·셸 프로필은 아니다). 선택자만
    // 실린다 — 키는 데몬이 실행 시점에 넣으므로 요청·지문·제목에 남지 않는다.
    if (shell.agent === "claude") {
      const routing = claudeProviderFor(
        "claude",
        usePreferences.getState(),
        useWorkbenchStore.getState().claudeProviderRouting,
      );
      if (!routing.ok) {
        // 구 데몬은 claude_provider를 모른다 — Anthropic으로 조용히 떨어지지
        // 않고 거절한다(사용자는 GLM으로 도는 줄 알고 Anthropic 사용량을 쓴다).
        this.detachedPendingLaunches.delete(leafId);
        const message = t(CLAUDE_PROVIDER_DAEMON_OUTDATED_KEY);
        useWorkbenchStore.getState().panePhase(leafId, "failed", message);
        this.toast(message);
        return;
      }
      if (routing.provider) request.claude_provider = routing.provider;
    }
    try {
      const launched = await this.launchWithFallbacks(request, shells, fallbackCwds);
      const { outcome } = launched;
      if (!outcome.session_id) throw new Error(t("terminal.session.notCreated"));
      if (!useWorkbenchStore.getState().panes[leafId]) {
        if (this.detachedPendingLaunches.delete(leafId)) {
          this.workloadSession.set(outcome.workload_id, outcome.session_id);
          this.liveSessions.add(outcome.session_id);
          return;
        }
        // 시작하는 동안 pane이 닫혔다 — 방금 생긴 session도 같이 끝낸다
        // (닫힌 터미널의 작업을 daemon에 남기지 않는다).
        await this.terminateWorkload(outcome.workload_id, outcome.session_id);
        return;
      }
      useWorkbenchStore.getState().paneSessionAssigned(leafId, outcome.session_id, outcome.workload_id);
      // 끝난 작업을 되살리는 실행이면 이 새 작업이 그 작업을 이어받는다(최근 종료에서 빠진다).
      const recoveredFrom = this.recoveryOrigins.get(leafId);
      if (recoveredFrom !== undefined) {
        this.recoveryOrigins.delete(leafId);
        useWorkbenchStore.getState().markWorkloadRecovered(recoveredFrom, outcome.workload_id);
      }
      useWorkbenchStore.getState().paneTitle(leafId, launched.shell.label, launched.cwd);
      // 대체 경로·셸로 열었으면 알린다 — 헤더만 바뀌면 왜 다른 곳·다른 셸로 열렸는지 모른다.
      const notices: string[] = [];
      if (launched.cwd !== cwd) {
        const key = launched.cwdReason === "cwd_permission_denied"
          ? "terminal.session.cwdFallbackDenied"
          : "terminal.session.cwdFallback";
        notices.push(t(key, { from: cwd, cwd: launched.cwd }));
      }
      if (launched.shell !== shell) {
        // 이름이 같으면("zsh" → "zsh": Homebrew zsh가 사라져 /bin/zsh로) 경로로 보여 준다 —
        // 같은 이름 두 개로는 무엇이 바뀌었는지 알 수 없다.
        const sameLabel = launched.shell.label === shell.label;
        notices.push(t("terminal.session.shellFallback", {
          shell: sameLabel ? shell.program : shell.label,
          fallback: sameLabel ? launched.shell.program : launched.shell.label,
        }));
      }
      if (notices.length > 0) this.toast(notices.join(" · "));
      this.workloadSession.set(outcome.workload_id, outcome.session_id);
      this.liveSessions.add(outcome.session_id);
      await this.attachPane(leafId, outcome.session_id);
    } catch (error) {
      this.detachedPendingLaunches.delete(leafId);
      useWorkbenchStore
        .getState()
        .panePhase(leafId, "failed", launchFailureMessage(error, { cwd, program: shell.program, resuming }));
    }
  }

  /**
   * 일반 셸의 후보: 고른 셸 → 플랫폼 기본 셸 → (POSIX) /bin/sh. 같은 실행 파일은 한 번만.
   * 대체 셸의 이름은 실행 파일 이름이다("zsh"·"sh") — 무엇으로 열렸는지 그대로 보인다.
   */
  private shellCandidates(shell: ShellSpec): ShellSpec[] {
    const platform = this.deps.platform;
    const candidates: ShellSpec[] = [shell];
    const add = (program: string, argv: string[]): void => {
      if (candidates.some(c => c.program === program)) return;
      candidates.push({ program, argv, label: program.slice(program.search(/[^\\/]*$/)) });
    };
    add(defaultShellProgram(platform), defaultShellArgv(platform));
    if (platform !== "windows") add("/bin/sh", []);
    return candidates;
  }

  /**
   * 셸 후보마다 경로 후보를 차례로 시도한다. 셸을 바꾸는 것은 그 셸이 없거나 뜨지
   * 못했을 때(PROGRAM_NOT_FOUND·SPAWN_FAILED)뿐이고, 다른 실패와 마지막 후보의
   * 실패는 그대로 던진다. 호스트 자원이 바닥난 SPAWN_FAILED(`spawn_host_exhausted` —
   * PTY·프로세스·파일 한도)는 어느 셸이든 같으므로 바꿔 보지 않는다: 시도마다 실패한
   * 작업만 쌓인다. 다시 보내는 요청은 새 request_id를 쓴다(내용이 다르다).
   */
  private async launchWithFallbacks(
    request: LaunchRequest,
    shells: readonly ShellSpec[],
    fallbackCwds: readonly string[],
  ): Promise<LaunchAttempt & { shell: ShellSpec }> {
    for (let index = 0; ; index += 1) {
      const shell = shells[index];
      const attempt = index === 0
        ? request
        : { ...request, request_id: this.uuid(), ...cleanShellCommand(this.deps.platform, shell) };
      try {
        return { ...(await this.launchInAvailableCwd(attempt, fallbackCwds)), shell };
      } catch (error) {
        const shellFailed = error instanceof RpcClientError &&
          (error.code === "PROGRAM_NOT_FOUND" ||
            (error.code === "SPAWN_FAILED" && error.details?.reason_code !== "spawn_host_exhausted"));
        if (!shellFailed || index + 1 >= shells.length) throw error;
      }
    }
  }

  /**
   * 시작 경로를 쓸 수 없으면(CWD_UNAVAILABLE) 다음 후보 경로로 다시 시작한다(04-ui §2-4).
   * 첫 거절의 사유(reason_code — 사라짐·권한 없음)를 함께 돌려 알림 문구를 고르게 한다.
   * 다른 실패와 마지막 후보의 실패는 그대로 던진다.
   */
  private async launchInAvailableCwd(
    request: LaunchRequest,
    fallbackCwds: readonly string[],
  ): Promise<LaunchAttempt> {
    let attempt = request;
    let cwdReason: string | null = null;
    for (const next of fallbackCwds) {
      try {
        return { outcome: await this.deps.client.workloadLaunch(attempt), cwd: attempt.cwd, cwdReason };
      } catch (error) {
        if (!(error instanceof RpcClientError) || error.code !== "CWD_UNAVAILABLE") throw error;
        cwdReason ??= error.details?.reason_code ?? "cwd_missing";
      }
      attempt = { ...attempt, request_id: this.uuid(), cwd: next };
    }
    return { outcome: await this.deps.client.workloadLaunch(attempt), cwd: attempt.cwd, cwdReason };
  }

  private readonly pendingWorkloadConnections = new Set<string>();
  private readonly recoveredWorkloadPanes = new Map<string, string>();

  /** 사용자가 연결을 누르면 살아 있는 PTY는 attach, 종료된 에이전트는 재개한다. */
  async attachWorkloadTerminal(
    workloadId: string,
    options?: { agentOnly?: boolean },
  ): Promise<void> {
    if (this.disposed || this.pendingWorkloadConnections.has(workloadId)) return;
    this.pendingWorkloadConnections.add(workloadId);
    try {
      const store = useWorkbenchStore.getState();
      const recoveredLeaf = this.recoveredWorkloadPanes.get(workloadId);
      const recovered = recoveredLeaf ? store.panes[recoveredLeaf] : null;
      // 재생이 막 끝나 새 셸을 띄우기 직전인 창은 잠깐 exited로 보인다 — 대기 중인 복구도 진행 중으로 본다.
      if (recovered && (this.pendingShellRecoveries.has(recovered.leafId) ||
        (recovered.phase !== "exited" && recovered.phase !== "failed"))) {
        this.focusLeaf(recovered.leafId);
        return;
      }
      const workload = store.workloads.find(w => w.workload_id === workloadId);
      const sessionId = this.workloadSession.get(workloadId) ?? workload?.session_id ?? null;
      if (workload && isFinishedWorkload(workload.state)) {
        // 끝난 작업은 되살린다: 대화 기록이 있으면 그 대화로(이미 실행 중이면 그 창으로),
        // 없으면 이전 출력을 재생한 창에서 새 셸로 잇는다. 새 작업은 관리 작업에 들어간다.
        const saved = Object.values(store.panes).find(p => p.workloadId === workloadId)?.resume;
        const target = await this.findRecoveryTarget(workloadId, sessionId, saved);
        if (this.disposed) return;
        if (target.kind === "running") {
          if (!target.live) {
            this.toast(t("terminal.resume.alreadyRunning"));
            return;
          }
          this.focusLiveConversation(target.live);
          if (target.live.workloadId) {
            useWorkbenchStore.getState().markWorkloadRecovered(workloadId, target.live.workloadId);
          }
          return;
        }
        if (target.kind === "resume") {
          const panes = useWorkbenchStore.getState().panes;
          const existing = Object.values(panes).find(p => p.workloadId === workloadId);
          await this.resumeAgentSession(target.info, existing ? { leafId: existing.leafId } : { newPane: true });
          const state = useWorkbenchStore.getState();
          const leafId = existing?.leafId ?? Object.keys(state.panes).find(id => !panes[id]);
          if (leafId) {
            this.recoveredWorkloadPanes.set(workloadId, leafId);
            this.linkRecovery(leafId, workloadId);
            const pane = state.panes[leafId];
            if (pane?.sessionId) this.focusSessionPane(pane.sessionId);
          }
          return;
        }
        // 일괄 재개는 대화 기록이 있는 작업만 되살린다 — 기록이 사라진
        // 터미널까지 새 셸로 잇지 않는다(개별 연결은 그대로 잇는다).
        if (options?.agentOnly) return;
        this.recoverShell(workload, sessionId);
        return;
      }
      if (!sessionId) {
        this.toast(t("terminal.session.noSessionToAttach"));
        return;
      }
      this.attachSessionToNewPane(sessionId, workloadId);
    } catch {
      this.toast(t("terminal.resume.launchFailed"));
    } finally {
      this.pendingWorkloadConnections.delete(workloadId);
    }
  }

  /** 일괄 재개가 도는 중인가 — 버튼 연타로 같은 대화를 두 번 띄우지 않는다. */
  private resumingAgents = false;

  /**
   * 끝난 에이전트 대화 여러 개를 차례로 되살린다(대기열의 "모두 재개").
   * 항목마다 개별 연결과 같은 경로를 쓰되 기록이 없는 작업은 건너뛴다
   * (agentOnly) — 일반 셸까지 새로 띄우지 않는다. 이미 실행 중인 대화는
   * 그 창으로 이동한다. 한 번에 프로세스를 여러 개 띄우므로 호출자가
   * 먼저 사용자 확인을 받는다(ModalState "resume-agents").
   */
  async resumeAgentWorkloads(workloadIds: string[]): Promise<void> {
    if (this.disposed || this.resumingAgents || workloadIds.length === 0) return;
    this.resumingAgents = true;
    let resumed = 0;
    let failed = 0;
    try {
      for (const workloadId of workloadIds) {
        if (this.disposed) return;
        await this.attachWorkloadTerminal(workloadId, { agentOnly: true });
        if (this.disposed) return;
        if (this.agentWorkloadRevived(workloadId)) resumed += 1;
        else failed += 1;
      }
    } finally {
      this.resumingAgents = false;
    }
    // 개별 실패 문구는 한 자리(toast)를 공유하므로 마지막에 요약으로 덮는다.
    this.toast(
      failed > 0
        ? t("queue.resumeAll.partial", { n: resumed, failed })
        : t("queue.resumeAll.done", { n: resumed }),
    );
  }

  /** 이 끝난 작업이 되살아났는가 — 새 창이 살아 있거나 실행 중 대화로 옮겨 갔다. */
  private agentWorkloadRevived(workloadId: string): boolean {
    const state = useWorkbenchStore.getState();
    // 실행 중이던 대화로 이동한 경우 그 연결이 기록된다(markWorkloadRecovered).
    if (rememberedWorkload(state.workloadMemory, workloadId)?.recoveredBy) return true;
    const leafId = this.recoveredWorkloadPanes.get(workloadId);
    const pane = leafId ? state.panes[leafId] : null;
    return Boolean(pane && pane.phase !== "exited" && pane.phase !== "failed");
  }

  /** pane(leaf id)별로 재생이 끝나면 새 셸로 이어 갈 복구. */
  private readonly pendingShellRecoveries = new Map<string, PendingShellRecovery>();
  /** pane(leaf id)별로 다음에 붙을 새 작업이 이어받을 끝난 작업. */
  private readonly recoveryOrigins = new Map<string, string>();

  /**
   * 에이전트 기록이 없는 끝난 터미널을 되살린다: 같은 창에 이전 출력을 재생한 뒤 원래
   * 경로에서 새 셸을 이어 띄운다. 그 창이 아직 열려 있으면 그 자리에서, 출력 저널이
   * 없으면(PTY를 잃은 중단) 새 창에 새 셸만 연다.
   */
  private recoverShell(workload: WorkloadSummary, sessionId: string | null): void {
    const workloadId = workload.workload_id;
    const cwd = this.pickCwd(workload.cwd || null);
    const panes = useWorkbenchStore.getState().panes;
    const existing = Object.values(panes).find(p => p.workloadId === workloadId);
    if (existing) {
      // 복구 중에 다시 눌러도 같은 창으로 간다(맨 위 recoveredWorkloadPanes 확인).
      this.recoveredWorkloadPanes.set(workloadId, existing.leafId);
      this.focusLeaf(existing.leafId);
      // 이전 출력을 아직 재생하는 중(연결 응답·복원 차례 대기 포함)이면 재생이 끝난 뒤 잇는다.
      // 끝난 작업이니 그 재생은 exited로 끝나야 한다 — 끝남을 모르는 pipeline에도 알린다.
      const pipeline = this.pipelines.get(existing.viewId);
      const replaying = existing.phase === "replaying" &&
        (!pipeline || pipeline.currentMode === "detached" || pipeline.currentMode === "replay");
      if (existing.sessionId && replaying) {
        pipeline?.markExited();
        this.pendingShellRecoveries.set(existing.leafId, { sessionId: existing.sessionId, fromWorkloadId: workloadId, cwd });
        return;
      }
      // 끝난 창·연결 실패, 그리고 끝남을 듣지 못해 살아 보이는 창은 기다릴 재생이 없다 —
      // 바로 새 셸로 잇는다(기다리면 오지 않을 끝남을 기다리며 죽은 창으로 남는다).
      void this.continueInFreshShell(existing.leafId, workloadId, cwd);
      return;
    }
    if (!sessionId) {
      if (!this.newTerminalAt(cwd)) return;
      const leafId = Object.keys(useWorkbenchStore.getState().panes).find(id => !panes[id]);
      if (leafId) {
        this.recoveredWorkloadPanes.set(workloadId, leafId);
        this.linkRecovery(leafId, workloadId);
      }
      return;
    }
    this.attachSessionToNewPane(sessionId, workloadId);
    const pane = Object.values(useWorkbenchStore.getState().panes).find(p => !panes[p.leafId] && p.sessionId === sessionId);
    if (!pane) return;
    this.recoveredWorkloadPanes.set(workloadId, pane.leafId);
    this.pendingShellRecoveries.set(pane.leafId, { sessionId, fromWorkloadId: workloadId, cwd });
  }

  /** 복구를 기다리는 pane의 재생이 끝났거나 저널을 열지 못했으면 새 셸로 잇는다. */
  private resumePendingShellRecovery(leafId: string, sessionId: string): void {
    const pending = this.pendingShellRecoveries.get(leafId);
    if (!pending || pending.sessionId !== sessionId) return;
    // 재생 pipeline의 콜백 안에서 불린다 — 그 pipeline을 버리는 일은 콜백이 끝난 뒤에 한다.
    // 대기 표시는 새 셸이 창을 잡을 때까지 남긴다: 그 사이의 연결 클릭이 같은 작업을 두 번 되살리지 않게.
    queueMicrotask(() => {
      if (this.pendingShellRecoveries.get(leafId) !== pending) return; // 그 사이 창을 닫았거나 이미 이었다.
      this.pendingShellRecoveries.delete(leafId);
      void this.continueInFreshShell(leafId, pending.fromWorkloadId, pending.cwd);
    });
  }

  /**
   * 끝난 세션을 보여 주던 창의 화면을 그대로 두고 새 셸로 잇는다. `launchCwd`는 그 작업을
   * 처음 실행한 경로다 — 셸이 마지막으로 알린 경로(재생한 OSC 7이 갱신한 pane cwd)가
   * 먼저이고, 그 경로가 사라졌으면 처음 경로 → 프로젝트 root → home 순으로 연다.
   */
  private async continueInFreshShell(leafId: string, fromWorkloadId: string, launchCwd: string): Promise<void> {
    const pane = useWorkbenchStore.getState().panes[leafId];
    if (!pane || this.disposed || this.pendingPaneRestarts.has(leafId)) return;
    this.pendingPaneRestarts.add(leafId);
    try {
      const cwds = this.cwdCandidates(pane.cwd, launchCwd);
      // 보존한 화면이 새 PTY의 첫 크기(80×24)로 접혔다 다시 펴지지 않게 지금 격자로 시작한다.
      const grid = this.terminalGrid(pane.viewId);
      this.resetPaneForFreshPty(leafId, pane, { keepView: true });
      this.deps.registry.get(pane.viewId)?.terminal.write(recoverySeparator());
      this.linkRecovery(leafId, fromWorkloadId);
      await this.launchShell(leafId, cwds[0], { fallbackCwds: cwds.slice(1), grid });
    } finally {
      this.pendingPaneRestarts.delete(leafId);
    }
  }

  /**
   * 이 창에서 시작하는(또는 이미 시작한) 새 작업이 끝난 작업을 이어받는다고 기록한다.
   * 새 작업이 붙는 순간 끝난 작업은 최근 종료에서 빠진다.
   */
  private linkRecovery(leafId: string, fromWorkloadId: string): void {
    const pane = useWorkbenchStore.getState().panes[leafId];
    if (pane?.workloadId && pane.workloadId !== fromWorkloadId) {
      useWorkbenchStore.getState().markWorkloadRecovered(fromWorkloadId, pane.workloadId);
      return;
    }
    this.recoveryOrigins.set(leafId, fromWorkloadId);
  }

  /** 기존 session을 새 pane으로 연결(재생 attach — QueueDrawer의 터미널 연결). */
  attachSessionToNewPane(sessionId: string, workloadId: string | null): void {
    const store = useWorkbenchStore.getState();
    // 세션당 view는 하나뿐이다(sessionIndex): 이미 pane이 붙어 있으면 두 번째
    // attach가 첫 pane의 출력 경로를 빼앗아 얼려 버리므로 그 pane으로 이동한다.
    if (Object.values(store.panes).some((p) => p.sessionId === sessionId)) {
      this.focusSessionPane(sessionId);
      const pane = Object.values(store.panes).find(p => p.sessionId === sessionId)!;
      if (!this.pipelines.has(pane.viewId)) void this.attachPane(pane.leafId, sessionId, workloadId);
      return;
    }
    const tab = store.tabs.find((t) => t.id === store.activeTabId);
    if (!tab || tab.kind !== "terminal") {
      // 새 터미널 탭을 열고 그리로 간다. addTab은 활성 탭이 없을 때만 활성으로 삼으므로
      // mission·agent-view 탭을 보던 중이면 직접 옮겨야 한다 — 안 그러면 다시 불린
      // 이 함수가 또 탭을 만들며 끝없이 되풀이한다.
      const tabId = this.uuid();
      store.addTab(tabId, t("terminal.reattach"));
      store.setActiveTab(tabId);
      if (useWorkbenchStore.getState().activeTabId !== tabId) return;
      this.attachSessionToNewPane(sessionId, workloadId);
      return;
    }
    const cwdSource = store.workloads.find(w => w.workload_id === workloadId)?.cwd ?? null;
    const leafId = this.uuid();
    const viewId = this.uuid();
    const cwd = this.pickCwd(cwdSource);
    if (!tab.root) {
      // 빈 탭: 첫 leaf로 붙인다.
      const root = makeLeaf(leafId, viewId, sessionId);
      useWorkbenchStore.setState((s) => ({
        tabs: s.tabs.map((t) => (t.id === tab.id && t.kind === "terminal" ? { ...t, root } : t)),
        panes: {
          ...s.panes,
          [leafId]: this.makePaneMeta(leafId, viewId, sessionId, workloadId, t("terminal.reattach"), cwd),
        },
        focusedLeafId: leafId,
      }));
      void this.attachPane(leafId, sessionId, workloadId);
      return;
    }
    if (!canAddPane(tab.root)) {
      this.toast(paneLimitText(MAX_PANES_PER_TAB));
      return;
    }
    const focusedLeafId = store.focusedLeafId ?? null;
    if (!focusedLeafId) return;
    const size = this.paneSizes.get(store.panes[focusedLeafId].viewId) ?? { width: 1e6, height: 1e6 };
    if (!canSplit(size, "row").ok) {
      this.toast(splitNoSpaceText());
      return;
    }
    const ok = useWorkbenchStore.getState().applySplit(
      tab.id,
      focusedLeafId,
      { leafId, viewId, sessionId, title: t("terminal.reattach"), cwd },
      this.uuid(),
      "row",
    );
    if (!ok) return;
    useWorkbenchStore.getState().panePhase(leafId, "replaying");
    void this.attachPane(leafId, sessionId, workloadId);
  }

  private makePaneMeta(
    leafId: string,
    viewId: string,
    sessionId: string | null,
    workloadId: string | null,
    title: string,
    cwd: string | null,
  ) {
    return {
      leafId,
      viewId,
      sessionId,
      workloadId,
      title,
      cwd,
      phase: "replaying" as PanePhase,
      error: null as string | null,
      usage: null,
      flowBlocked: false,
      agent: null,
      relief: PANE_RELIEF_NONE,
      protected: false,
    };
  }

  /**
   * store가 아는 이 세션의 작업이 이미 끝났는가. 작업이 세션 id를 알리면 그것으로만
   * 맞추고, 세션 id를 주지 않는 데몬의 작업만 작업 id로 맞춘다.
   */
  private sessionKnownFinished(sessionId: string, workloadId: string | null): boolean {
    return useWorkbenchStore.getState().workloads.some(w => isFinishedWorkload(w.state) &&
      (w.session_id ? w.session_id === sessionId : workloadId !== null && w.workload_id === workloadId));
  }

  /** attach의 재생 바이트 예산 — 살아 있는 세션은 작게, 끝난 세션은 넉넉히. */
  private replayBudget(sessionId: string, workloadId: string | null): number {
    return this.sessionKnownFinished(sessionId, workloadId)
      ? REPLAY_BUDGET_FINISHED_BYTES
      : REPLAY_BUDGET_LIVE_BYTES;
  }

  private async attachPane(leafId: string, sessionId: string, workloadId: string | null = null): Promise<void> {
    if (this.disposed) return;
    const pane = useWorkbenchStore.getState().panes[leafId];
    if (!pane) return;
    if (workloadId) {
      useWorkbenchStore.getState().paneSessionAssigned(leafId, sessionId, workloadId);
    }
    const entry = this.deps.registry.acquire(pane.viewId);
    useWorkbenchStore.getState().paneReplayTrimmed(leafId, null);
    const pipeline = new SessionPipeline(
      {
        client: this.deps.client,
        sessionId,
        viewId: pane.viewId,
        terminal: entry.terminal,
        uuid: this.uuid,
        // live 출력을 한 프레임(16 ms)만큼 모아 한 번에 쓴다. Ink 계열 TUI(Claude
        // Code 등)는 프레임을 `ESC[H`로 커서를 올린 뒤 줄마다 지우고 다시 쓰는데,
        // PTY 읽기(1 KiB)·저널 flush·브리지 전달이 그 프레임을 여러 레코드로 쪼갠다.
        // 조각마다 xterm에 쓰면 사이사이 반쯤 지운 프레임이 그려져 새 출력마다
        // 화면이 위아래로 떨린다. 16 ms는 키 입력 에코에서 느껴지지 않는다.
        outputCoalesceMs: this.deps.platform === "windows" ? 32 : LIVE_OUTPUT_COALESCE_MS,
        // 화면 지우기: 스크롤백까지 지우고, 기억해 둔 지점이 있으면 재생 때 같은 자리에서 다시 지운다.
        clearTerminal: () => clearTerminalHistory(entry.terminal),
        clearAfterSeq: loadClearMark(sessionId),
        // 마지막 출력 배지는 화면 글자가 바뀐 출력만 센다(screenOutput.ts).
        screenSignature: () => screenSignature(entry.terminal),
        // 잘린 재생 뒤 에이전트 CLI는 자기 영역만 다시 그린다 — 화면을 지우고 폭을 흔든다.
        repaintTrimmedReplay: () => Boolean(useWorkbenchStore.getState().panes[leafId]?.agent),
      },
      {
        // live 출력마다 활동 점(3초 창)을 켠다. 재생 레코드는 세지 않는다.
        onLiveOutput: () => markTerminalActivity(sessionId),
        // 마지막 출력 시각(경과 배지): 화면 글자를 바꾼 live 출력만 — 마우스 모드
        // 재설정·크기 변경 뒤 다시 그리기 같은 이벤트성 바이트로 0초가 되지 않게.
        onScreenOutput: () => recordSessionOutput(sessionId),
        onCleared: (afterSeq) => {
          saveClearMark(sessionId, afterSeq);
          // 화면이 레코드 없이 바뀌었다 — 같은 seq라도 스냅샷을 다시 뜨게 한다.
          this.snapshotMarks.delete(sessionId);
        },
        // 잘린 헤드부터의 재생: pane 헤더에 "앞부분 N MiB 지워짐"을 남긴다.
        onReplayTrimmed: (droppedBytes) => useWorkbenchStore.getState().paneReplayTrimmed(leafId, droppedBytes),
        onModeChange: (mode, previous) => {
          if (this.disposed) return;
          if (mode === "replay") {
            useWorkbenchStore.getState().panePhase(leafId, "replaying");
            // 다시 재생으로 들어갔다 — live로 버틴 시간 재기를 접는다.
            this.cancelHealthyTimer(leafId);
          }
          if (mode === "live") {
            useWorkbenchStore.getState().panePhase(leafId, "live");
            // strikes·재접속 backoff는 live를 잠시 버틴 뒤에만 지운다 — 곧바로
            // 다시 끊기는 재생이 상한을 피해 영원히 되풀이되지 않게.
            this.armHealthyTimer(leafId);
          }
          if (mode === "exited") {
            // 재생은 replaying을 거치며 사유를 지운다 — 데몬 재시작으로 끝난 세션이면 다시 적는다.
            const reason = this.daemonRestartedSessions.has(sessionId) ? t("terminal.session.daemonRestarted") : null;
            useWorkbenchStore.getState().panePhase(leafId, "exited", reason);
            this.liveSessions.delete(sessionId);
            this.resumePendingShellRecovery(leafId, sessionId);
            this.cancelHealthyTimer(leafId);
            this.replayStallStrikes.delete(leafId);
            this.clearReattachBackoff(leafId);
          }
          if (previous === "replay" && mode !== "replay") {
            this.foregroundReplayDone(leafId);
            // 길게 재생했으면 곧바로 떠 둔다 — 다음 시작(또는 곧 닥칠 비정상 종료)이 같은 재생을 반복하지 않게.
            if (pipeline.replayedBytes >= SNAPSHOT_AFTER_REPLAY_BYTES) {
              setTimeout(() => void this.captureReplaySnapshot(pane.viewId), 0);
            }
          }
        },
        onReplayStalled: () => {
          // 재생이 끊겼다(레코드가 오지 않는다). 연결이 살아 있어도 아무도 이
          // pipeline을 살리지 못하므로 여기서 다시 붙는다.
          void this.recoverStalledReplay(pane.viewId, leafId, sessionId);
        },
        onInputError: (error) => this.recoverFromInputFailure(pane.viewId, sessionId, error),
        onInputDelivered: () => setInputIssue(sessionId, null),
        onExit: () => {
          useWorkbenchStore.getState().panePhase(leafId, "exited");
        },
        onLocalInput: (data) => this.broadcastToSiblings(leafId, data),
        onOutputText: (text) => this.handleOutputText(leafId, sessionId, text),
      },
    );
    this.pipelines.set(pane.viewId, pipeline);
    this.sessionIndex.set(sessionId, { viewId: pane.viewId, leafId });
    // 작업이 이미 끝난 세션은 재생 뒤 live가 아니라 exited로 끝낸다. attach 응답에
    // `exited`가 없는 데몬(그 필드 이전 빌드)에서도 끝난 PTY가 입력을 받는 살아 있는
    // 창처럼 남지 않고, 이 재생을 기다리는 복구(새 셸로 잇기)도 이어진다.
    if (this.sessionKnownFinished(sessionId, workloadId ?? pane.workloadId)) pipeline.markExited();
    // OSC 0/2(동적 제목)·OSC 7(cwd) 보고(W1-3/4). 재생 중에 같은
    // 시퀀스가 다시 와도 값 설정이라 멱등이다 — 재접속 후 제목·cwd가
    // 저널 그대로 복원된다.
    registerTerminalReporting(entry.terminal, {
      onTitle: (title) => useWorkbenchStore.getState().paneTerminalTitle(leafId, title),
      onCwd: (cwd) => {
        // OSC 7은 셸이 주는 대로 믿지 않는다 — 이 플랫폼의 절대 경로만
        // 다음 pane의 시작 cwd 후보로 기록한다(WSL의 /home/u는 버림).
        const accepted = acceptReportedCwd(cwd, this.deps.platform);
        if (accepted) useWorkbenchStore.getState().paneCwd(leafId, accepted);
      },
    });
    this.deps.registry.setFitHandler(pane.viewId, (dims: FitDimensions) => {
      this.paneSizes.set(pane.viewId, { width: dims.width, height: dims.height });
      pipeline.requestResize(dims.cols, dims.rows);
    });
    try {
      // 붙기 전에 한 번 재 둔다: 최신 fit이 attach 응답과 함께 곧바로 데몬에
      // 가도록(크기 기록이 저널 꼬리에 실려 재생이 끝나는 자리에서 바로 펴진다).
      entry.refit();
      const snapshot = await this.loadReplaySnapshot(leafId, sessionId, entry.terminal);
      if (this.disposed || this.pipelines.get(pane.viewId) !== pipeline) return;
      const result = await pipeline.attach(false, snapshot, {
        maxReplayBytes: this.replayBudget(sessionId, workloadId ?? pane.workloadId),
      });
      if (snapshot && Number(result.replay_from_seq) === snapshot.seq + 1) {
        this.snapshotMarks.set(sessionId, { seq: snapshot.seq, at: Date.now() });
      }
      if (this.disposed || this.pipelines.get(pane.viewId) !== pipeline) return;
      // Mount/ResizeObserver may finish before launch installs the fit handler,
      // or its resize request may expire while attach has no epoch yet.
      // Refit once the session can accept it, through the ordered resize path.
      entry.refit();
    } catch (error) {
      if (this.disposed) return;
      // 그 사이 이 pane이 새 view로 다시 붙었거나(재생 중 수동 재시도) 닫혔으면 지난 시도의
      // 실패다 — 늦게 온 거절(attach 시간 초과 등)이 지금 붙어 있는 view를 실패로 덮지 않게 한다.
      if (this.pipelines.get(pane.viewId) !== pipeline) return;
      useWorkbenchStore
        .getState()
        .panePhase(leafId, "failed", errorMessage(error, t("terminal.session.attachFailed")));
      this.foregroundReplayDone(leafId);
      // 이전 출력을 못 열었어도 복구는 새 셸로 잇는다.
      this.resumePendingShellRecovery(leafId, sessionId);
    }
  }

  // ------------------------------------------------------- replay snapshots

  /**
   * 이 세션의 저장된 화면 스냅샷. 찾으면 직렬화에 담기지 않는 상태(OSC 제목, CLI의
   * 다시 그리기 방식)를 먼저 되살린다 — 스냅샷이 쓰이지 않고 전체 재생으로 가더라도
   * 같은 세션이 보고했던 값이다.
   */
  private async loadReplaySnapshot(leafId: string, sessionId: string, terminal: unknown): Promise<ReplaySnapshot | null> {
    const store = this.deps.replaySnapshots;
    if (!store || !this.deps.serializeTerminal) return null;
    let snapshot: ReplaySnapshot | null;
    try {
      snapshot = await store.load(sessionId);
    } catch {
      return null;
    }
    if (!snapshot || this.disposed) return null;
    if (snapshot.terminalTitle && useWorkbenchStore.getState().panes[leafId]?.sessionId === sessionId) {
      useWorkbenchStore.getState().paneTerminalTitle(leafId, snapshot.terminalTitle);
    }
    if (snapshot.historyRebuilder && terminal && typeof terminal === "object") markHistoryRebuilder(terminal);
    return snapshot;
  }

  /** 지금 화면을 스냅샷으로 뜬다(저장하지 않음). 레코드 경계가 아니면 null. */
  private currentSnapshot(viewId: string): (PipelineSnapshot & { historyRebuilder: boolean }) | null {
    const serialize = this.deps.serializeTerminal;
    const pipeline = this.pipelines.get(viewId);
    const entry = this.deps.registry.get(viewId);
    if (!serialize || !pipeline || !entry || this.disposed) return null;
    const seq = pipeline.snapshotSeq;
    if (seq === null) return null;
    let state: { data: string; cols: number; rows: number } | null;
    try {
      // 직렬화는 동기다 — 그 사이에 레코드가 적용되어 seq와 어긋날 수 없다.
      state = serialize(entry.terminal);
    } catch {
      state = null;
    }
    if (!state || state.data.length > REPLAY_SNAPSHOT_MAX_CHARS) return null;
    return { seq, ...state, clearMark: pipeline.clearMark, historyRebuilder: isHistoryRebuilder(entry.terminal) };
  }

  /**
   * 이 view의 화면을 스냅샷으로 저장한다. 마지막 저장 뒤로 바뀐 레코드가 없거나 지금이
   * 레코드 경계가 아니면 아무것도 하지 않는다(다음 기회에 뜬다).
   */
  private captureReplaySnapshot(viewId: string): Promise<void> | null {
    const store = this.deps.replaySnapshots;
    const pipeline = this.pipelines.get(viewId);
    if (!store || !pipeline) return null;
    const sessionId = pipeline.sessionId;
    const ref = this.sessionIndex.get(sessionId);
    if (ref?.viewId !== viewId) return null;
    const seq = pipeline.snapshotSeq;
    const mark = this.snapshotMarks.get(sessionId);
    if (seq === null || (mark && mark.seq >= seq)) return null;
    const snapshot = this.currentSnapshot(viewId);
    if (!snapshot) return null;
    const now = Date.now();
    this.snapshotMarks.set(sessionId, { seq: snapshot.seq, at: now });
    const pane = useWorkbenchStore.getState().panes[ref.leafId];
    const saving = store.save({
      sessionId,
      seq: snapshot.seq,
      cols: snapshot.cols,
      rows: snapshot.rows,
      data: snapshot.data,
      clearMark: snapshot.clearMark ?? 0,
      terminalTitle: pane?.terminalTitle ?? null,
      historyRebuilder: snapshot.historyRebuilder,
      savedAt: now,
    }).catch(() => undefined);
    this.snapshotSaves.add(saving);
    void saving.finally(() => this.snapshotSaves.delete(saving));
    return saving;
  }

  /**
   * 주기 점검: 바뀐 세션 중 가장 오래전에 뜬 하나만 뜬다. 직렬화는 메인 스레드에서
   * 돌므로 한 번에 여러 pane을 뜨지 않고, 계속 바뀌는 세션도 최소 간격을 둔다.
   */
  private snapshotTick(): void {
    if (this.disposed) return;
    const now = Date.now();
    let stalest: { viewId: string; at: number } | null = null;
    for (const [viewId, pipeline] of this.pipelines) {
      const seq = pipeline.snapshotSeq;
      if (seq === null) continue;
      const mark = this.snapshotMarks.get(pipeline.sessionId);
      if (mark && (mark.seq >= seq || now - mark.at < SNAPSHOT_MIN_INTERVAL_MS)) continue;
      const at = mark?.at ?? 0;
      if (!stalest || at < stalest.at) stalest = { viewId, at };
    }
    if (stalest) void this.captureReplaySnapshot(stalest.viewId);
  }

  /** 바뀐 화면을 모두 뜨고 저장이 끝나기를(상한까지) 기다린다 — 창 숨김·종료 직전. */
  private async flushReplaySnapshots(timeoutMs = SNAPSHOT_QUIT_FLUSH_MS): Promise<void> {
    if (!this.deps.replaySnapshots || !this.deps.serializeTerminal) return;
    for (const viewId of [...this.pipelines.keys()]) void this.captureReplaySnapshot(viewId);
    const pending = [...this.snapshotSaves];
    if (pending.length === 0) return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    await Promise.race([
      Promise.all(pending),
      new Promise<void>((resolve) => { timer = setTimeout(resolve, timeoutMs); }),
    ]);
    if (timer !== null) clearTimeout(timer);
  }

  /** 지금 열린 pane의 세션만 남기고 스냅샷을 정리한다(앱 시작 복원 뒤 한 번). */
  private pruneReplaySnapshots(): void {
    const store = this.deps.replaySnapshots;
    if (!store) return;
    const keep = new Set<string>();
    for (const pane of Object.values(useWorkbenchStore.getState().panes)) {
      if (pane.sessionId) keep.add(pane.sessionId);
    }
    void store.retain(keep).catch(() => undefined);
  }

  private readonly pendingPaneRestarts = new Set<string>();

  retryPane(leafId: string, options: { newShell?: boolean } = {}): void {
    const pane = useWorkbenchStore.getState().panes[leafId];
    if (!pane || this.disposed || this.pendingPaneRestarts.has(leafId)) return;
    // 사용자가 직접 재시도하는 것은 막힌 재생을 푸는 탈출구다 — 재생 중이라도 허용한다.
    if (pane.phase === "starting") return;
    if (pane.phase === "failed" && pane.sessionId) {
      // 연결 실패(데몬 재시작·타임아웃) 뒤의 재시도: 스토어의 작업 상태가 낡았을 수
      // 있으니 최신 스냅샷으로 먼저 판단한다 — 끝난 에이전트 대화면 곧바로 이어서
      // 연다(다시 붙었다가 종료 오버레이의 "이어서 열기"를 또 누르게 하지 않는다).
      this.pendingPaneRestarts.add(leafId);
      void this.retryFailedPane(pane, options.newShell ?? false)
        .finally(() => this.pendingPaneRestarts.delete(leafId));
      return;
    }
    const workload = useWorkbenchStore.getState().workloads.find(w => w.workload_id === pane.workloadId);
    if (pane.sessionId && pane.phase !== "exited" && !(workload && isFinishedWorkload(workload.state))) {
      this.reattachWithFreshView(pane);
      return;
    }
    this.pendingPaneRestarts.add(leafId);
    void this.restartExitedPane(pane, options.newShell ?? false).finally(() => this.pendingPaneRestarts.delete(leafId));
  }

  /** 같은 세션에 새 view로 다시 붙는다(수동 재시도·연결 실패 뒤 살아 있는 세션). */
  private reattachWithFreshView(pane: PaneMeta): void {
    if (!pane.sessionId) return;
    const leafId = pane.leafId;
    this.pipelines.get(pane.viewId)?.dispose();
    this.pipelines.delete(pane.viewId);
    this.releaseSessionRoute(pane.sessionId, pane.viewId);
    this.disposeView(pane.viewId);
    // 버리는 view는 데몬에서도 뗀다. 컨트롤 연결이 살아 있는 동안 데몬은 view를
    // 스스로 치우지 않으므로, 떼지 않으면 재시도마다 세션의 view 상한(2)을 하나씩
    // 먹어 세 번째부터 "session already has the maximum number of views"로 영영
    // 붙지 못한다. 같은 연결의 RPC는 순서대로 처리되므로 기다리지 않아도 아래
    // attach보다 먼저 빠진다.
    this.detachView(pane.sessionId, pane.viewId);
    // 수동 재시도는 깨끗한 상태에서 시작한다(막힘 strikes·재접속 backoff).
    this.replayStallStrikes.delete(leafId);
    this.clearReattachBackoff(leafId);
    this.cancelHealthyTimer(leafId);
    const viewId = this.uuid();
    useWorkbenchStore.setState(s => ({ panes: { ...s.panes, [leafId]: { ...pane, viewId, phase: "replaying", error: null } } }));
    useWorkbenchStore.getState().patchLeaf(leafId, { view_id: viewId });
    void this.attachPane(leafId, pane.sessionId, pane.workloadId);
  }

  /**
   * 연결 실패("세션에 연결하지 못했습니다") 뒤의 재시도. 실패 시점의 스토어는 데몬이
   * 재시작됐는지 모른다(복구가 스냅샷을 받기 전에 실패했다) — 먼저 최신 스냅샷을
   * 받아 작업이 끝났는지 본다.
   *  - 끝났고 이어서 열 수 있는 에이전트 대화(claude·codex·opencode)면 곧바로
   *    이어서 연다 — 클릭 한 번.
   *  - 끝났고 "새 셸"을 눌렀으면 새 PTY를 연다.
   *  - 그 밖(아직 살아 있거나 일반 셸)은 새 view로 다시 붙는다(기록 재생).
   * 데몬이 아직 안 올라왔으면 실패 오버레이를 유지한다(다시 재시도할 수 있게).
   */
  private async retryFailedPane(pane: PaneMeta, newShell: boolean): Promise<void> {
    const leafId = pane.leafId;
    const samePane = (): PaneMeta | null => {
      const current = useWorkbenchStore.getState().panes[leafId];
      return current && current.viewId === pane.viewId && current.sessionId === pane.sessionId ? current : null;
    };
    useWorkbenchStore.getState().panePhase(leafId, "replaying");
    try {
      const snapshot = await this.deps.client.systemSnapshot();
      if (this.disposed || !samePane()) return;
      useWorkbenchStore.getState().applySnapshot(snapshot);
      this.refreshPaneAgents();
    } catch (error) {
      if (this.disposed || !samePane()) return;
      useWorkbenchStore
        .getState()
        .panePhase(leafId, "failed", errorMessage(error, t("terminal.session.attachFailed")));
      return;
    }
    const current = samePane();
    if (!current) return;
    const workload = useWorkbenchStore.getState().workloads.find(w => w.workload_id === current.workloadId);
    // 데몬이 모르는 작업(재시작으로 사라짐)도 끝난 것으로 본다.
    const finished = workload ? isFinishedWorkload(workload.state) : current.workloadId !== null;
    if (finished) {
      // 끝난 작업의 PTY는 더 이상 살아 있지 않다. 연결 실패(markAttachedPanesFailed)는
      // liveSessions를 비우지 않으므로 여기서 지운다 — 남겨 두면 "재생 중"으로 바꾼 이 창이
      // 자기 대화를 "다른 곳에서 실행 중"으로 잡아 이어서 열지 못한다.
      if (current.sessionId) this.liveSessions.delete(current.sessionId);
      const found = newShell
        ? null
        : await this.findResumableSession(current.workloadId, current.sessionId, current.resume);
      if (this.disposed || !samePane()) return;
      // 같은 대화가 다른 창에서 실행 중이면 그 창으로 옮겨 가지 않는다(restartExitedPane과
      // 같다) — 아래에서 이 창의 기록을 다시 붙여 종료 오버레이로 끝낸다.
      const resume = found && !this.activeAgentSession(found, current.workloadId) ? found : null;
      if (resume) {
        if (current.workloadId) this.linkRecovery(leafId, current.workloadId);
        await this.resumeAgentSession(resume, { leafId });
        // 재개가 이 창을 잡지 못했으면(실행 파일 없음·쓸 수 없는 세션 id — 사유는 알림으로
        // 떴다) "재생 중"에 두지 않고 실패 오버레이로 돌려 다시 시도할 수 있게 한다.
        if (!this.disposed && samePane()?.phase === "replaying") {
          useWorkbenchStore
            .getState()
            .panePhase(leafId, "failed", pane.error ?? t("terminal.session.attachFailed"));
        }
        return;
      }
      if (newShell) {
        await this.restartExitedPane(current, true);
        return;
      }
    }
    // 살아 있는 세션(일시적 실패)이거나 이어서 열 대화가 없는 셸: 기록을 다시 붙인다.
    this.reattachWithFreshView(current);
  }

  private async restartExitedPane(pane: PaneMeta, newShell: boolean): Promise<void> {
    const found = newShell ? null : await this.findResumableSession(pane.workloadId, pane.sessionId, pane.resume);
    const current = useWorkbenchStore.getState().panes[pane.leafId];
    if (this.disposed || current?.viewId !== pane.viewId || current.sessionId !== pane.sessionId) return;
    // 같은 대화가 다른 창에서 이미 실행 중이면(재개 기록이 있는 창이라도) 이 창은 일반
    // 터미널과 같다 — 그 창으로 옮겨 가지도, 아무 일 없이 끝나지도 않고 이 자리에서 새 셸을 연다.
    const resume = found && !this.activeAgentSession(found, pane.workloadId) ? found : null;
    // 이 자리에서 새로 시작하는 실행이 끝난 작업을 이어받는다(최근 종료에서 뺀다).
    if (pane.workloadId) this.linkRecovery(pane.leafId, pane.workloadId);
    if (resume) {
      await this.resumeAgentSession(resume, { leafId: pane.leafId });
      return;
    }
    // 일반 셸 또는 명시적인 새 셸은 새 PTY를 사용한다. 마지막 경로가 사라졌으면 대체 경로로 연다.
    // 재개에 실패한 창은 아직 경로가 없을 수 있다 — 그 대화의 경로(→ 상위)를 잇는다.
    const cwds = this.cwdCandidates(current.cwd, current.resume?.cwd);
    this.resetPaneForFreshPty(pane.leafId, current);
    await this.launchShell(pane.leafId, cwds[0], { fallbackCwds: cwds.slice(1) });
  }

  /**
   * 끝난 세션 자리를 새 PTY로 갈아 끼울 준비: 파이프라인·출력 경로·뷰를
   * 버리고 pane을 빈 상태(새 viewId, session/workload 없음, starting)로
   * 되돌린다. 이 시점이 사용자가 새 실행을 시작한 순간이므로 "이어서 열
   * 수 있음" 기록도 함께 지운다 — 재개 오버레이가 남아 있지 않게.
   */
  private resetPaneForFreshPty(leafId: string, pane: PaneMeta, options: { keepView?: boolean } = {}): void {
    this.pipelines.get(pane.viewId)?.dispose();
    this.pipelines.delete(pane.viewId);
    this.releaseSessionRoute(pane.sessionId, pane.viewId);
    this.forgetSessionText(pane.sessionId);
    if (pane.workloadId) this.workloadSession.delete(pane.workloadId);
    if (pane.sessionId) this.liveSessions.delete(pane.sessionId);
    if (pane.sessionId) {
      const ref = this.sessionIndex.get(pane.sessionId);
      if (ref && ref.leafId === leafId) this.sessionIndex.delete(pane.sessionId);
      // 데몬에서도 이 view를 뗀다. 브리지 채널만 놓으면 데몬은 끝난 세션을 여전히 "보는 중"으로
      // 여겨 출력 펌프를 20ms마다 돌리고, 저널 보존 정리에서도 빼 연결이 끊길 때까지 붙들어 둔다.
      this.detachView(pane.sessionId, pane.viewId);
    }
    // keepView: 화면(xterm)과 그 안의 출력은 두고 세션만 비운다 — 최근 종료 복구가
    // 재생한 출력 아래에서 새 셸을 잇는다. 이전 pipeline에 묶인 fit handler만 푼다.
    let newViewId = pane.viewId;
    if (options.keepView) {
      this.deps.registry.setFitHandler(pane.viewId, null);
    } else {
      this.disposeView(pane.viewId);
      newViewId = this.uuid();
    }
    useWorkbenchStore.setState((s) => ({
      panes: {
        ...s.panes,
        [leafId]: {
          ...pane,
          viewId: newViewId,
          sessionId: null,
          workloadId: null,
          phase: "starting",
          error: null,
          agent: null,
        },
      },
    }));
    // 위 setState는 불러온 쪽이 들고 있던 pane을 펼치므로 resume이 그대로
    // 남는다 — 지우는 것은 그 뒤에 store 동작으로 한다(한 곳에서만 지운다).
    useWorkbenchStore.getState().paneResume(leafId, null);
    useWorkbenchStore.getState().patchLeaf(leafId, { view_id: newViewId, session_id: null });
  }

  /**
   * 기록된 에이전트 세션을 사용자가 연결·재실행할 때 이어서 연다.
   *
   * - program: 기록된 절대 경로 → 없으면 시스템 probe. 둘 다 없으면 토스트.
   * - argv: `claude --resume <id>` / `opencode --session <id>` / `codex resume <id>`(codex는 하위 명령이
   *   먼저), 자율 실행 설정이 켜져 있으면 그 플래그가 붙는다.
   * - cwd: 반드시 기록된 cwd. 포커스된 pane의 cwd를 쓰지 않는다.
   * 실행은 기존 셸 경로(launchShell → mode "shell")를 그대로 탄다.
   */
  private readonly pendingAgentResumes = new Set<string>();

  async resumeAgentSession(
    info: AgentResumeInfo,
    target: { leafId: string } | { newPane: true },
  ): Promise<void> {
    if (this.disposed) return;
    const key = `${info.agent}:${info.agentSessionId}`;
    if (this.pendingAgentResumes.has(key)) return;
    const pane = "leafId" in target ? useWorkbenchStore.getState().panes[target.leafId] : null;
    // 끝났거나 연결에 실패한 pane 자신의 작업은 "실행 중인 대화"로 세지 않는다 —
    // 낡은 스토어가 그 작업을 아직 RUNNING으로 알아도 같은 자리에 이어서 연다.
    const active = this.activeAgentSession(
      info,
      pane?.phase === "exited" || pane?.phase === "failed" ? pane.workloadId : null,
    );
    if (active) {
      this.focusLiveConversation(active);
      return;
    }
    this.pendingAgentResumes.add(key);
    try {
      await this.launchAgentResume(info, target);
    } finally {
      this.pendingAgentResumes.delete(key);
    }
  }

  private async launchAgentResume(
    info: AgentResumeInfo,
    target: { leafId: string } | { newPane: true },
  ): Promise<void> {
    const originalViewId = "leafId" in target ? useWorkbenchStore.getState().panes[target.leafId]?.viewId : null;
    // 인자를 먼저 만든다: 플래그로 해석될 세션 id("-…")나 경로처럼 보이는
    // 값은 여기서 멈춘다 — 기록이 오염됐을 때의 방어선(isSafeSessionId).
    const argv = resumeArgv(
      info.agent,
      info.agentSessionId,
      autonomyEnabled(info.agent, usePreferences.getState()),
    );
    if (!argv) {
      this.toast(t("terminal.resume.launchFailed"));
      return;
    }
    let program = info.program;
    if (!program) {
      try {
        program = (await this.deps.resolveAgentProgram?.(info.agent)) ?? null;
      } catch {
        program = null;
      }
    }
    if (this.disposed) return;
    if (!program) {
      this.toast(t("terminal.resume.noProgram", { name: agentDisplayName(info.agent) }));
      return;
    }
    const shell: ShellSpec = { program, argv, label: agentDisplayName(info.agent), agent: info.agent };
    const toastBefore = useWorkbenchStore.getState().toast;
    try {
      if ("leafId" in target) {
        const pane = useWorkbenchStore.getState().panes[target.leafId];
        if (!pane || pane.viewId !== originalViewId) return;
        this.resetPaneForFreshPty(target.leafId, pane);
        useWorkbenchStore.getState().paneResume(target.leafId, info);
        this.nextShell = shell;
        await this.launchShell(target.leafId, info.cwd);
        return;
      }
      const before = useWorkbenchStore.getState().panes;
      this.nextShell = shell;
      const created = this.newTerminalAt(info.cwd);
      if (created) {
        const pane = Object.values(useWorkbenchStore.getState().panes).find(p => !before[p.leafId]);
        if (pane) useWorkbenchStore.getState().paneResume(pane.leafId, info);
      }
      // 분할 한도·공간 부족으로 pane이 생기지 않았으면 다음 새 터미널이
      // 이 셸을 물려받지 않도록 되돌린다(launchShell은 즉시 비운다).
      if (this.nextShell === shell) this.nextShell = null;
      // 자리를 못 만들었는데 아무 문구도 없으면(포커스된 pane 없음 등)
      // 사용자에게는 아무 일도 안 일어난 것처럼 보인다 — 그때만 알린다.
      // 한도·공간 부족은 splitFocused가 더 구체적으로 말했으니 덮지 않는다.
      if (!created && useWorkbenchStore.getState().toast === toastBefore) {
        this.toast(t("terminal.resume.launchFailed"));
      }
    } catch {
      // launchShell은 스스로 pane에 실패를 남기지만, 그 바깥이 깨지면 아무
      // 표시도 남지 않는다 — 재개가 실패했다는 것만이라도 알린다.
      if (this.nextShell === shell) this.nextShell = null;
      this.toast(t("terminal.resume.launchFailed"));
    }
  }

  /**
   * newTerminal과 같되 cwd를 강제한다(기록된 경로에서 재개).
   * 반환값은 "pane을 만들었는가" — 호출자가 조용한 실패를 알릴 수 있게.
   */
  private newTerminalAt(cwd: string): boolean {
    const store = useWorkbenchStore.getState();
    const tab = store.tabs.find((t) => t.id === store.activeTabId) ?? null;
    if (!tab || tab.kind !== "terminal" || !tab.root) {
      return this.createFirstPane(tab?.kind === "terminal" ? tab.id : this.createTab(), cwd);
    }
    return this.splitFocused("row", cwd);
  }

  // -------------------------------------------------------------- pane close

  /** User-facing close requests honor the saved preference or ask once per group. */
  requestClosePanes(leafIds: string[], tabId?: string): void {
    const panes = leafIds.map(id => useWorkbenchStore.getState().panes[id]).filter(Boolean);
    if (!panes.length || panes.every(p => p.phase === "exited") || usePreferences.getState().terminateOnClose) {
      void this.confirmClosePanes(leafIds, true, tabId);
      return;
    }
    useWorkbenchStore.getState().openModal({ kind: "close-panes", leafIds, tabId });
  }

  /** Closing the entire workspace is destructive enough to always require an explicit choice. */
  requestCloseAllTabs(): void {
    const state = useWorkbenchStore.getState();
    if (!state.tabs.length) return;
    // mission 탭은 닫기 확인에 들어가지 않는다(05 §10: close all은 mission을
    // 숨긴다) — 확인 대상은 일반 terminal pane뿐이다.
    const leafIds = state.tabs.flatMap((tab) =>
      tab.kind === "terminal" && tab.root ? listLeaves(tab.root).map((leaf) => leaf.id) : [],
    );
    if (leafIds.length === 0) {
      // 종료 확인이 필요한 일반 terminal이 없다: mission 숨김만 남는다 → 바로 실행.
      for (const tab of [...useWorkbenchStore.getState().tabs]) {
        useWorkbenchStore.getState().closeTab(tab.id);
      }
      return;
    }
    state.openModal({ kind: "close-panes", leafIds, closeAllTabs: true });
  }

  async confirmClosePanes(
    leafIds: string[],
    terminate: boolean,
    tabId?: string,
    closeAllTabs = false,
  ): Promise<void> {
    for (const leafId of leafIds) await this.closePane(leafId, terminate);
    if (closeAllTabs) {
      for (const tab of [...useWorkbenchStore.getState().tabs]) {
        useWorkbenchStore.getState().closeTab(tab.id);
      }
    } else if (tabId) {
      useWorkbenchStore.getState().closeTab(tabId);
    }
  }

  async closePane(leafId: string, terminate = true): Promise<void> {
    this.pendingShellRecoveries.delete(leafId);
    this.recoveryOrigins.delete(leafId);
    const pane = useWorkbenchStore.getState().panes[leafId];
    if (!pane) return;
    if (!terminate && pane.phase === "starting" && !pane.sessionId) this.detachedPendingLaunches.add(leafId);
    const pipeline = this.pipelines.get(pane.viewId);
    pipeline?.dispose();
    this.pipelines.delete(pane.viewId);
    this.releaseSessionRoute(pane.sessionId, pane.viewId);
    this.forgetSessionText(pane.sessionId);
    if (pane.sessionId) {
      const ref = this.sessionIndex.get(pane.sessionId);
      if (ref && ref.leafId === leafId) this.sessionIndex.delete(pane.sessionId);
      try {
        await this.deps.client.sessionDetach({ session_id: pane.sessionId, view_id: pane.viewId });
      } catch {
        // detach 실패는 종료와 화면 닫기를 막지 않는다.
      }
    }
    if (terminate && pane.workloadId) await this.terminateWorkload(pane.workloadId, pane.sessionId);
    this.disposeView(pane.viewId);
    useWorkbenchStore.getState().applyClose(leafId);
  }

  /**
   * 브리지의 view별 출력 채널을 놓아준다 — pipeline을 버릴 때마다(닫기·
   * 재시도·컨트롤러 폐기). 안 그러면 Rust 채널과 웹뷰 콜백이 pane 수명을
   * 넘어 영구히 남는다(장기 실행 누수의 프론트 쪽 절반).
   */
  private releaseSessionRoute(sessionId: string | null, viewId: string): void {
    if (!sessionId) return;
    try {
      this.deps.client.sessionUnsubscribe?.({ session_id: sessionId, view_id: viewId });
    } catch {
      // 전송 실패는 무시한다 — 재접속이 표를 통째로 비운다.
    }
  }

  /** 데몬에서 이 view를 뗀다(기다리지 않는다). */
  private detachView(sessionId: string, viewId: string): void {
    try {
      void Promise.resolve(this.deps.client.sessionDetach({ session_id: sessionId, view_id: viewId }))
        .catch(() => undefined);
    } catch {
      // 떼지 못해도 새 PTY로 넘어가는 일은 막지 않는다 — 연결이 끊기면 데몬이 view를 치운다.
    }
  }

  /** 세션에 묶인 기록(패턴 감지용 꼬리·쿨다운, 데몬 재시작 표시)을 세션과 함께 버린다. */
  private forgetSessionText(sessionId: string | null): void {
    if (!sessionId) return;
    this.patternCooldown.clear(sessionId);
    this.patternTails.delete(sessionId);
    this.daemonRestartedSessions.delete(sessionId);
  }

  // ------------------------------------------------------------------ quit

  /** 살아 있는 작업(터미널) id — 종료 확인의 근거이자 "터미널도 종료"의 대상. */
  activeWorkloadIds(): string[] {
    const state = useWorkbenchStore.getState();
    const ids = new Set<string>();
    for (const workload of state.workloads) {
      if (!isFinishedWorkload(workload.state)) ids.add(workload.workload_id);
    }
    for (const pane of Object.values(state.panes)) {
      if (pane.workloadId && pane.phase !== "exited" && pane.phase !== "failed") {
        ids.add(pane.workloadId);
      }
    }
    return [...ids];
  }

  /**
   * 앱 종료 요청(트레이 종료·앱 메뉴 Quit·팔레트). Rust 워치독을 먼저 풀고,
   * 살아 있는 터미널이 없거나 설정(quitBehavior)이 정해져 있으면 바로
   * 결정하며, 그 밖에는 묻는다. 대화상자가 이미 떠 있으면 그대로 둔다.
   */
  requestQuit(): void {
    if (this.disposed) return;
    void this.deps.quit?.ack();
    if (this.quitInFlight) return;
    const active = this.activeWorkloadIds();
    const behavior = usePreferences.getState().quitBehavior;
    if (active.length === 0 || behavior === "keep") {
      void this.confirmQuit(false);
      return;
    }
    if (behavior === "terminate") {
      void this.confirmQuit(true);
      return;
    }
    const store = useWorkbenchStore.getState();
    if (store.modal?.kind === "quit") return;
    store.openModal({ kind: "quit", sessions: active.length });
    // 트레이에 숨어 있던 창이면 대화상자가 보이도록 앞으로 가져온다.
    void this.deps.quit?.reveal();
  }

  /**
   * 종료 결정. `terminate`면 살아 있는 작업을 모두 강제 종료하고 완료를 확인한 뒤
   * 저장 배치를 비우고 종료한다 — 다음 실행이 죽은 세션을 복원하려 들지
   * 않게. 유지면 배치는 그대로 두어 다음 실행이 다시 연결한다.
   */
  confirmQuit(terminate: boolean): Promise<void> {
    if (this.quitInFlight) return this.quitInFlight;
    this.quitInFlight = (async () => {
      if (terminate) await this.terminateAllForQuit();
      // 터미널을 남겨 두고 끝낼 때만: 다음 시작이 저널 전체를 다시 재생하지 않도록 화면을 저장한다.
      else await this.flushReplaySnapshots();
      const store = useWorkbenchStore.getState();
      if (store.modal?.kind === "quit") store.closeModal();
      await this.deps.quit?.exit();
    })().catch((error) => {
      this.toast(errorMessage(error, t("quit.failed")));
    }).finally(() => {
      this.quitInFlight = null;
    });
    return this.quitInFlight;
  }

  /** 종료 결정이 진행 중인가 — 진행 중엔 바깥 클릭으로 취소하지 않는다. */
  get quitPending(): boolean {
    return this.quitInFlight !== null;
  }

  /** 대화상자 취소 — Rust 쪽 대기 상태도 함께 푼다. */
  cancelQuit(): void {
    const store = useWorkbenchStore.getState();
    if (store.modal?.kind === "quit") store.closeModal();
    void this.deps.quit?.cancel();
  }

  private async terminateAllForQuit(): Promise<void> {
    let timer: ReturnType<typeof setTimeout> | null = null;
    const deadline = new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error(t("quit.failed"))), QUIT_CANCEL_TIMEOUT_MS);
    });
    let expired = false;
    const requested = new Set<string>();
    const terminate = async () => {
      // Include terminals detached from this window and newly discovered workloads.
      while (!expired) {
        const snapshot = await this.deps.client.systemSnapshot();
        if (expired) return;
        const active = snapshot.workloads.filter((workload) => !isFinishedWorkload(workload.state));
        if (active.length === 0) return;
        await Promise.all(active.filter((workload) => !requested.has(workload.workload_id)).map(async (workload) => {
          await this.deps.client.workloadCancel({
            request_id: this.uuid(), workload_id: workload.workload_id, force: true,
          });
          requested.add(workload.workload_id);
        }));
        if (!expired) await new Promise((resolve) => setTimeout(resolve, 100));
      }
    };
    try {
      await Promise.race([terminate(), deadline]);
    } finally {
      expired = true;
      if (timer !== null) clearTimeout(timer);
    }
    for (const [viewId, pipeline] of this.pipelines) {
      pipeline.dispose();
      this.releaseSessionRoute(pipeline.sessionId, viewId);
      this.deps.registry.setFitHandler(viewId, null);
    }
    this.pipelines.clear();
    this.sessionIndex.clear();
    this.workloadSession.clear();
    this.liveSessions.clear();
    this.patternTails.clear();
    useWorkbenchStore.setState({ tabs: [], panes: {}, activeTabId: null, focusedLeafId: null, modal: null });
  }

  /**
   * 세션 종료 요청. 실패해도 화면 정리는 계속하고(닫기를 막지 않는다)
   * 남은 작업은 대기열 drawer에서 다시 종료할 수 있게 문구만 띄운다.
   */
  private async terminateWorkload(workloadId: string, sessionId: string | null): Promise<void> {
    try {
      await this.deps.client.workloadCancel({ request_id: this.uuid(), workload_id: workloadId });
      if (sessionId) this.liveSessions.delete(sessionId);
      this.workloadSession.delete(workloadId);
    } catch (error) {
      this.toast(errorMessage(error, t("terminal.workload.cancelFailed")));
    }
  }

  private disposeView(viewId: string): void {
    this.deps.registry.setFitHandler(viewId, null);
    this.deps.registry.get(viewId)?.dispose();
    this.paneSizes.delete(viewId);
  }

  // ----------------------------------------------------------- workloads UI

  /**
   * pane 메뉴의 수동 완화(08-pressure-relief §2). 그 pane의 세션 id를 찾아
   * `session.relief`를 부르고, 응답을 곧바로 pane에 반영한다 — 다음 snapshot을
   * 기다리지 않아 사용자가 고른 즉시 배지가 바뀐다. 세션이 없는 pane(시작 전·
   * 종료됨)은 데몬에 물어볼 대상이 없어 조용히 넘긴다. 실패는 다른 작업 동작과
   * 같은 자리(toast)로 알린다.
   */
  async paneRelief(leafId: string, action: ReliefAction): Promise<void> {
    const sessionId = useWorkbenchStore.getState().panes[leafId]?.sessionId ?? null;
    if (!sessionId) return;
    try {
      const result = await this.deps.client.sessionRelief({ session_id: sessionId, action });
      useWorkbenchStore
        .getState()
        .paneRelief(leafId, paneReliefFrom(result.relief), result.protected);
    } catch (error) {
      this.toast(errorMessage(error, t("terminal.relief.failed")));
    }
  }

  /**
   * 자동 양보 정책 토글(08 §2 `relief.set_policy`). 데몬이 적용한 값을
   * 그대로 스토어에 반영한다(요청값이 아니라 응답값이 정본이다).
   */
  async setAutoYield(autoYield: boolean): Promise<void> {
    try {
      const policy = await this.deps.client.reliefSetPolicy({ auto_yield: autoYield });
      useWorkbenchStore.getState().setReliefPolicy(policy);
    } catch (error) {
      this.toast(errorMessage(error, t("terminal.relief.policyFailed")));
    }
  }

  async cancelWorkload(workloadId: string): Promise<void> {
    try {
      await this.deps.client.workloadCancel({ request_id: this.uuid(), workload_id: workloadId });
    } catch (error) {
      this.toast(errorMessage(error, t("terminal.workload.cancelFailed")));
    }
  }

  /** 자원 가드 수동 일시정지(08 §5) — 수동 정지는 자동 재개 대상이 아니다. */
  async suspendWorkload(workloadId: string): Promise<void> {
    try {
      await this.deps.client.workloadSuspend({ request_id: this.uuid(), workload_id: workloadId });
    } catch (error) {
      this.toast(errorMessage(error, t("terminal.workload.suspendFailed")));
    }
  }

  /** 자원 가드 일시정지 해제(08 §5). */
  async resumeWorkload(workloadId: string): Promise<void> {
    try {
      await this.deps.client.workloadResume({ request_id: this.uuid(), workload_id: workloadId });
    } catch (error) {
      this.toast(errorMessage(error, t("terminal.workload.resumeFailed")));
    }
  }

  /**
   * 일시정지 작업 전체 재개(08 §5) — 단축키(Cmd/Ctrl+Shift+R)·메뉴·팔레트가 부른다.
   * 가드가 SUSPENDED로 표시한 작업을 병렬로 `workload.resume`에 보내고, 결과는
   * 건별 toast 대신 한 번에 알린다(패널이 닫혀 있어도 눌렀는지 알 수 있게). 가드
   * 상태의 화면 반영은 데몬의 workload.changed 이벤트가 곧바로 올린다. 일시정지
   * 중인 작업이 없으면 조용히 지나간다.
   */
  async resumeAllSuspended(): Promise<void> {
    const suspended = useWorkbenchStore
      .getState()
      .workloads.filter((workload) => workload.guard?.kind === "SUSPENDED");
    if (suspended.length === 0) return;
    // 건별 catch는 두지 않는다 — allSettled가 모으면 아래에서 한 번에 알린다.
    const results = await Promise.allSettled(
      suspended.map((workload) =>
        this.deps.client.workloadResume({ request_id: this.uuid(), workload_id: workload.workload_id }),
      ),
    );
    const failed = results.filter((result) => result.status === "rejected").length;
    const resumed = suspended.length - failed;
    this.toast(
      failed > 0
        ? t("terminal.workload.resumeAllPartial", { n: resumed, failed })
        : t("terminal.workload.resumeAllDone", { n: resumed }),
    );
  }

  async reprioritize(workloadId: string, priority: number): Promise<void> {
    try {
      const result = await this.deps.client.workloadReprioritize({ workload_id: workloadId, priority });
      useWorkbenchStore.getState().setQueue(result.queue);
    } catch (error) {
      this.toast(errorMessage(error, t("terminal.workload.reprioritizeFailed")));
    }
  }

  async runManaged(input: {
    program: string;
    argv: string[];
    cwd: string;
    priority: number;
    reservationBytes: string;
    title?: string;
    /** Claude 제공자 선택자(claudeProviderFor 결과). 없거나 null이면 라우팅하지 않는다. */
    claudeProvider?: ClaudeProvider | null;
  }): Promise<void> {
    const request: LaunchRequest = {
      request_id: this.uuid(),
      profile_id: input.title ?? "managed",
      cwd: input.cwd,
      program: input.program,
      argv: input.argv,
      env_overrides: {},
      mode: "managed",
      executor: { kind: "local" },
      cols: 80,
      rows: 24,
      priority: input.priority,
      policy: { ...defaultShellPolicy(), reservation_bytes: input.reservationBytes },
    };
    if (input.claudeProvider) request.claude_provider = input.claudeProvider;
    try {
      await this.deps.client.workloadLaunch(request);
      useWorkbenchStore.getState().toggleQueueDrawer(true);
    } catch (error) {
      this.toast(errorMessage(error, t("terminal.workload.managedFailed")));
    }
  }

  // -------------------------------------------------------- shortcuts/clipboard

  dispatchShortcut(action: ShortcutAction): void {
    switch (action) {
      case "split-row":
        this.splitFocused("row");
        break;
      case "split-column":
        this.splitFocused("column");
        break;
      case "close-pane": {
        const focused = useWorkbenchStore.getState().focusedLeafId;
        if (focused) this.requestClosePanes([focused]);
        break;
      }
      case "palette":
        useWorkbenchStore.getState().openModal({ kind: "palette" });
        break;
      case "queue-toggle":
        useWorkbenchStore.getState().toggleQueueDrawer();
        break;
      case "layout-editor": {
        // 배치 편집 화면 열기/닫기(04-ui §2-6) — 같은 조합으로 돌아온다.
        const store = useWorkbenchStore.getState();
        store.setPage(store.page === "layout" ? "terminal" : "layout");
        break;
      }
      case "broadcast-toggle":
        this.toggleBroadcast();
        break;
      case "search":
        useWorkbenchStore.getState().openModal({ kind: "search" });
        break;
      case "copy":
        void this.copySelection();
        break;
      case "paste":
        void this.pasteFromClipboard();
        break;
      case "zoom-in":
        this.zoomFocused(1);
        break;
      case "zoom-out":
        this.zoomFocused(-1);
        break;
      case "zoom-reset":
        this.zoomFocused(0);
        break;
      case "next-tab":
      case "prev-tab":
        // 트랙패드 가로 스와이프와 같은 경로·같은 설정(끝에서 순환할지).
        useWorkbenchStore
          .getState()
          .cycleTab(action === "next-tab" ? 1 : -1, usePreferences.getState().tabSwipe.wrap);
        break;
      case "new-mission":
        // 보고 있는 터미널의 저장소로 새 AI 작업 대화상자(쓸 수 없으면 사유 안내, 숨김 빌드면 무시).
        requestNewMission();
        break;
      case "resume-all":
        void this.resumeAllSuspended();
        break;
      default:
        break;
    }
  }

  /**
   * 포커스된 pane의 글꼴 크기 조정(Ctrl/Cmd+= · - · 0). pane 단위로
   * 적용되고, 크기 변화는 setFontSize → refit → session.resize 경로를
   * 타고 PTY cols/rows까지 재협상된다. 상·하한에서는 no-op.
   */
  zoomFocused(step: ZoomStep): void {
    const store = useWorkbenchStore.getState();
    const pane = store.focusedLeafId ? store.panes[store.focusedLeafId] : undefined;
    if (!pane) return;
    const entry = this.deps.registry.get(pane.viewId);
    if (!entry) return;
    const current = entry.terminal.options?.fontSize ?? usePreferences.getState().baseFontSize;
    // 리셋(0)은 사용자가 저장한 기본 글자 크기로 돌아간다(W1-6).
    const target =
      step === 0 ? usePreferences.getState().baseFontSize : zoomFontSize(current, step);
    entry.setFontSize(clampFontSize(target));
  }

  private focusedTerminal() {
    const focused = useWorkbenchStore.getState().focusedLeafId;
    if (!focused) return null;
    const pane = useWorkbenchStore.getState().panes[focused];
    if (!pane) return null;
    return this.deps.registry.get(pane.viewId)?.terminal ?? null;
  }

  async copySelection(): Promise<boolean> {
    const term = this.focusedTerminal();
    if (!term) return false;
    const selection = term.hasSelection() ? term.getSelection() : "";
    if (!selection) return false;
    try {
      await navigator.clipboard.writeText(selection);
      return true;
    } catch {
      return false;
    }
  }

  /**
   * 클립보드를 직접 읽어 붙여넣는다 — 오른쪽 클릭 메뉴·팔레트·재정의한 단축키용.
   * WebKit(macOS)은 다른 앱이 복사한 내용을 이렇게 읽을 때마다 "붙여넣기" 확인
   * 말풍선을 띄우므로, 기본 단축키(Cmd+V 등)는 이 경로를 타지 않고 웹뷰의
   * 네이티브 paste 이벤트로 받는다(nativePaste.ts → pasteText).
   */
  async pasteFromClipboard(): Promise<void> {
    if (useWorkbenchStore.getState().modal) return;
    // 그림만 든 클립보드는 임시 파일 경로로 바꿔 넣는다(pasteImage).
    if (await this.pasteClipboardImage()) return;
    let text: string;
    try {
      text = await navigator.clipboard.readText();
    } catch {
      this.toast(t("terminal.clipboard.readFailed"));
      return;
    }
    this.pasteText(text);
  }

  /**
   * 이미 받은 클립보드 텍스트를 붙여넣는다(leafId가 없으면 초점 pane). 어느
   * 입구로 들어와도 1 MiB 한도·정화·bracketed paste·동시 입력 규칙은 같다.
   */
  pasteText(text: string, leafId?: string): void {
    const store = useWorkbenchStore.getState();
    if (store.modal || !text) return;
    const decision = inspectPaste(text);
    if (decision.kind === "reject") {
      store.openModal({ kind: "notice", message: pasteTooLargeText(1048576) });
      return;
    }
    this.sendPaste(text, leafId ?? store.focusedLeafId);
  }

  /**
   * 창에 떨어뜨린 파일 경로를 그 자리 pane에 붙여넣는다. 터미널의 오랜
   * 관례이고, 여기서는 그림·로그를 에이전트에게 건네는 가장 빠른 길이다.
   * 붙여넣은 pane으로 초점을 옮긴다 — 방금 넣은 곳에 이어서 치게.
   */
  pasteDroppedPaths(paths: readonly string[], leafId: string): void {
    const payload = dropPayload(paths, this.deps.platform);
    if (!payload) return;
    useWorkbenchStore.getState().focusPane(leafId);
    this.pasteText(payload, leafId);
  }

  /**
   * 그림 하나를 임시 파일로 떨구고 그 경로를 붙여넣는다(clipboardImage.ts).
   *
   * `true`는 "그림으로 처리했다"는 뜻이다 — 붙여넣었거나, 실패를 이미
   * 알렸거나. `false`면 이 앱이 그림을 다룰 수 없다는 뜻이라 호출자는 글
   * 경로로 되돌아간다.
   */
  async pasteImage(image: PasteImageSource, leafId?: string): Promise<boolean> {
    const port = this.deps.savePasteImage;
    if (!port) return false;
    let bytes: Uint8Array;
    try {
      bytes = new Uint8Array(await image.arrayBuffer());
    } catch {
      return false;
    }
    if (bytes.byteLength === 0) return false;
    if (bytes.byteLength > PASTE_IMAGE_MAX_BYTES) {
      this.toast(t("terminal.paste.imageTooLarge", { max: PASTE_IMAGE_MAX_BYTES / (1024 * 1024) }));
      return true;
    }
    try {
      const path = await port.save(bytes, image.type);
      this.pasteText(quoteDropPath(path, this.deps.platform), leafId);
      return true;
    } catch {
      this.toast(t("terminal.paste.imageFailed"));
      return true;
    }
  }

  /**
   * 클립보드가 그림만 들고 있으면 그림으로 처리한다. 글이 함께 있으면
   * (브라우저에서 복사할 때 흔하다) 글이 이긴다 — 터미널 붙여넣기는 원래
   * 글을 넣는 동작이고, 그림은 글이 없을 때의 대안이어야 놀랍지 않다.
   *
   * `navigator.clipboard.read`는 엔진·권한에 따라 없거나 거절될 수 있다.
   * 그때는 false로 돌아가 기존 `readText` 경로를 그대로 탄다.
   */
  private async pasteClipboardImage(): Promise<boolean> {
    const canRead =
      typeof navigator !== "undefined" && typeof navigator.clipboard?.read === "function";
    if (!this.deps.savePasteImage || !canRead) return false;
    try {
      for (const item of await navigator.clipboard.read()) {
        if (item.types.includes("text/plain")) return false;
        const type = pickImageType(item.types);
        if (type === null) continue;
        return await this.pasteImage(await item.getType(type));
      }
    } catch {
      return false;
    }
    return false;
  }

  private sendPaste(text: string, leafId: string | null): void {
    const pane = leafId ? useWorkbenchStore.getState().panes[leafId] : undefined;
    const pipeline = pane ? this.pipelines.get(pane.viewId) : undefined;
    if (!leafId || !pipeline) return;
    // xterm의 paste()와 같은 규칙: 개행은 CR로, 괄호는 이 pane의 셸이
    // DECSET 2004를 켰을 때만(cmd·PowerShell 5는 마커를 문자 그대로 받는다).
    const data = preparePaste(text, bracketedPasteEnabled(this.paneTerminal(leafId)));
    // 붙여넣기는 1 MiB까지 허용되므로 큐 여유를 기다리는 대용량 경로로
    // 보낸다(W1-8: 64 KiB 큐 상한과의 정합성).
    void pipeline.sendLargeInput(data);
    this.broadcastToSiblings(leafId, data);
  }

  /**
   * 동기 입력 토글(단축키·상단 바 버튼·팔레트·띠의 끄기 버튼 공용 진입점).
   * 켜진 상태는 상단 띠와 pane 테두리가 상시로 알리므로 toast는 띄우지
   * 않는다 — 같은 말을 두 번 하지 않는다.
   */
  toggleBroadcast(on?: boolean): void {
    useWorkbenchStore.getState().toggleBroadcastInput(on);
  }

  /**
   * 동기 입력(04 §1): 켜져 있으면 같은 탭의 다른 pane에도 같은 입력을 보낸다.
   * 원본 pane은 이미 자기 입력을 보냈으므로 건너뛴다. 보이지 않는 탭에는
   * 절대 보내지 않는다 — 화면에 없는 셸에 명령이 들어가면 안 된다.
   * live가 아닌 pane은 pipeline이 스스로 막는다(replay 중 입력 금지, 02 §5-2).
   */
  private broadcastToSiblings(originLeafId: string, data: string): void {
    const store = useWorkbenchStore.getState();
    if (!store.broadcastInput) return;
    if (isTerminalReport(data)) return;
    const tab = store.tabs.find((t) => (t.kind === "terminal" ? findLeaf(t.root, originLeafId) !== null : false));
    if (!tab || tab.kind !== "terminal" || !tab.root) return;
    // 괄호 붙여넣기는 받는 pane의 셸이 그 모드를 켰을 때만 감싼 채 보낸다
    // — 원본 pane의 모드가 형제에게도 맞으리란 보장이 없다.
    const unwrapped = unwrapBracketedPaste(data);
    for (const leaf of listLeaves(tab.root)) {
      if (leaf.id === originLeafId) continue;
      const pane = store.panes[leaf.id];
      if (!pane) continue;
      const sibling = this.deps.registry.get(pane.viewId)?.terminal;
      const payload = unwrapped !== null && !bracketedPasteEnabled(sibling) ? unwrapped : data;
      // 형제 전달도 대용량 경로로: 붙여넣기 배분이 큐 상한에 걸리지
      // 않게 한다(실패 시 pipeline이 onInputError로 알린다).
      void this.pipelines.get(pane.viewId)?.sendLargeInput(payload);
    }
  }

  hasSelectionInFocusedPane(): boolean {
    return this.focusedTerminal()?.hasSelection() ?? false;
  }

  // ------------------------------------------------------ pane context menu
  // 메뉴는 초점 pane이 아니라 오른쪽 클릭한 pane을 가리킨다 — leafId로 받는다.

  private paneTerminal(leafId: string) {
    const pane = useWorkbenchStore.getState().panes[leafId];
    if (!pane) return null;
    return this.deps.registry.get(pane.viewId)?.terminal ?? null;
  }

  paneHasSelection(leafId: string): boolean {
    return this.paneTerminal(leafId)?.hasSelection() ?? false;
  }

  /**
   * 앱(TUI)이 마우스 보고를 켰는가. 켜져 있으면 xterm이 마우스 누름을 앱으로
   * 보내므로, 오른쪽 클릭의 주인을 정할 때 쓴다(paneContextMenu.rightClickRouting).
   */
  paneMouseTrackingActive(leafId: string): boolean {
    const mode = this.paneTerminal(leafId)?.modes?.mouseTrackingMode;
    return mode !== undefined && mode !== "none";
  }

  /** 마우스 아래 링크(http/https) — 링크 위에서 연 메뉴만 링크 항목을 보인다. */
  paneHoveredLink(leafId: string): string | null {
    const term = this.paneTerminal(leafId);
    return term ? hoveredLink(term) : null;
  }

  paneFontSize(leafId: string): number {
    return this.paneTerminal(leafId)?.options?.fontSize ?? usePreferences.getState().baseFontSize;
  }

  selectAllInPane(leafId: string): void {
    this.paneTerminal(leafId)?.selectAll?.();
  }

  /**
   * 이 창의 화면·scrollback을 스크롤로 돌아갈 수 없게 지운다. 셸에는 아무것도
   * 보내지 않아 실행 중인 프로그램은 영향이 없다. 붙어 있는 pipeline이 지운
   * 지점을 기억해, 재접속·재시작 뒤의 저널 재생도 같은 자리에서 다시 지운다.
   */
  clearPaneScrollback(leafId: string): void {
    const pane = useWorkbenchStore.getState().panes[leafId];
    const pipeline = pane ? this.pipelines.get(pane.viewId) : undefined;
    if (pipeline) {
      pipeline.clearScreen();
      return;
    }
    clearTerminalHistory(this.paneTerminal(leafId));
  }

  /** 입력 초점을 그 pane의 터미널로 돌려준다(메뉴를 닫은 뒤). */
  focusPaneTerminal(leafId: string): void {
    const term = this.paneTerminal(leafId);
    if (!term) return;
    if (term.focus) {
      term.focus();
      return;
    }
    (term.element as HTMLElement | null)?.querySelector<HTMLTextAreaElement>("textarea")?.focus();
  }

  /** 메뉴의 "…복사" 항목 공용 — 성공·실패를 toast로 알린다. */
  async copyText(text: string): Promise<boolean> {
    try {
      await navigator.clipboard.writeText(text);
      this.toast(t("terminal.contextMenu.copied"));
      return true;
    } catch {
      this.toast(t("terminal.contextMenu.copyFailed"));
      return false;
    }
  }

  /**
   * 메모리 진단(W1-11): 살아 있는 터미널·scrollback 설정·JS 힙을 모아
   * 클립보드로 복사한다. 사용자가 매트릭스 실측 근거로 이슈에 붙인다.
   */
  copyMemoryDiagnostics(): boolean {
    const state = useWorkbenchStore.getState();
    const prefs = usePreferences.getState();
    const diagnostics = collectMemoryDiagnostics(state, prefs);
    const text = formatMemoryDiagnostics(diagnostics);
    try {
      void navigator.clipboard?.writeText(text);
      this.toast(t("diagnostics.memory.copied", { count: diagnostics.liveTerminals }));
      return true;
    } catch {
      return false;
    }
  }

  /** 현재 scrollback 검색(다음/이전 결과 이동 — 04 §6 R1). */
  searchFocused(query: string, direction: "next" | "previous"): boolean {
    const term = this.focusedTerminal();
    if (!term || !this.deps.searchHandler || !query) return false;
    return this.deps.searchHandler(term, query, direction);
  }

  /** 저널 전체 검색(W2): 스크롤백 밖 과거 출력까지 찾는다. */
  async searchJournals(query: string): Promise<SessionSearchResult> {
    return this.deps.client.sessionSearch({ query, case_sensitive: false });
  }

  /** 저널 검색 결과 클릭 → 해당 세션의 pane으로 이동(살아 있을 때만). */
  focusSessionPane(sessionId: string): void {
    const pane = Object.values(useWorkbenchStore.getState().panes).find((p) => p.sessionId === sessionId);
    if (pane) this.focusLeaf(pane.leafId);
  }

  /** 그 pane이 든 탭을 열고 초점을 준다(떠 있는 대화상자는 닫는다). */
  private focusLeaf(leafId: string): void {
    const state = useWorkbenchStore.getState();
    const tab = state.tabs.find((t) => t.kind === "terminal" && t.root && listLeaves(t.root).some((leaf) => leaf.id === leafId));
    if (tab) state.setActiveTab(tab.id);
    state.focusPane(leafId);
    useWorkbenchStore.getState().closeModal();
  }

  // ------------------------------------------------------------------ toast

  toast(message: string): void {
    // 워크벤치 flashToast와 같은 소유 토큰을 쓴다 — 서로의 4초 타이머가 나중에 뜬 토스트를 지우지 않는다.
    const token = showOwnedToast(message);
    if (this.toastTimer) clearTimeout(this.toastTimer);
    this.toastTimer = setTimeout(() => {
      this.toastTimer = null;
      clearOwnedToast(token, message);
    }, 4000);
  }
}

/**
 * 복원 시 고아 세션(저장된 배치에 없는 살아 있는 작업)을 cwd별로 묶는다
 * (04-ui §2-5). 같은 cwd는 한 탭 — 이름은 경로의 마지막 조각. 혼자인 작업은
 * 예전처럼 작업 제목을 그대로 쓴다. 탭당 상한을 넘으면 번호를 붙여 나눈다.
 */
export function groupOrphansByCwd(
  workloads: WorkloadSummary[],
  maxPerTab = MAX_PANES_PER_TAB,
): Array<{ title: string; workloads: WorkloadSummary[] }> {
  const buckets = new Map<string, WorkloadSummary[]>();
  for (const workload of workloads) {
    const key = workload.cwd ? `cwd:${workload.cwd}` : `one:${workload.workload_id}`;
    const bucket = buckets.get(key);
    if (bucket) bucket.push(workload);
    else buckets.set(key, [workload]);
  }
  const groups: Array<{ title: string; workloads: WorkloadSummary[] }> = [];
  for (const list of buckets.values()) {
    if (list.length === 1) {
      groups.push({ title: list[0].title, workloads: list });
      continue;
    }
    const base = projectTitle(list[0].cwd);
    const chunks = Math.ceil(list.length / maxPerTab);
    for (let i = 0; i < chunks; i += 1) {
      groups.push({
        title: chunks === 1 ? base : `${base} ${i + 1}`,
        workloads: list.slice(i * maxPerTab, (i + 1) * maxPerTab),
      });
    }
  }
  return groups;
}

/**
 * 계약의 `ReliefState`를 pane 표현으로 옮긴다(08 §2). `since_ms`는 U64 wire
 * string이라 여기서 한 번만 숫자로 바꾸고, 읽을 수 없으면 null이다 — 시간은
 * 툴팁에만 쓰이므로 값이 없다고 배지를 숨기지는 않는다.
 */
function paneReliefFrom(state: ReliefState | null | undefined): PaneRelief {
  if (!state || state.kind !== "YIELDED") return PANE_RELIEF_NONE;
  const since = Number(state.since_ms);
  return {
    kind: "YIELDED",
    sinceMs: Number.isFinite(since) ? since : null,
    manual: state.manual,
    partial: state.partial,
  };
}

function errorMessage(error: unknown, fallback: string): string {
  if (error instanceof RpcClientError && error.details?.reason_code === "claude_transcript_missing") {
    return t("terminal.resume.claudeTranscriptMissing");
  }
  // 제공자 라우팅 거절(zai_key_missing 등)은 reason_code만으로 문구를 만든다 —
  // 데몬 message를 그대로 보이지 않는다.
  const routed = claudeProviderErrorMessage(error);
  if (routed) return routed;
  if (error instanceof RpcClientError) return `${fallback} (${error.code}): ${error.message}`;
  if (error instanceof Error && error.message) return error.message;
  return fallback;
}

/**
 * 세션 시작 실패 문구. 대체 경로·셸까지 다 써도 실패했을 때만 여기에 온다 — 흔한
 * 사유는 무엇을 하면 되는지까지 말하고, 진단용 코드는 끝에 남긴다. 나머지는
 * `errorMessage`의 형식 그대로다.
 */
function launchFailureMessage(
  error: unknown,
  context: { cwd: string; program: string; resuming: boolean },
): string {
  const failed = t("terminal.session.launchFailed");
  if (!(error instanceof RpcClientError)) return errorMessage(error, failed);
  let hint: string | null = null;
  switch (error.code) {
    case "CWD_UNAVAILABLE":
      hint = context.resuming
        ? t("terminal.resume.cwdUnavailable", { cwd: context.cwd })
        : t("terminal.session.cwdUnavailable", { cwd: context.cwd });
      break;
    case "PROGRAM_NOT_FOUND":
      hint = t("terminal.session.programMissing", { program: context.program });
      break;
    case "SPAWN_FAILED":
      hint = t("terminal.session.spawnFailedHint");
      break;
    case "SESSION_LIMIT":
      hint = t("terminal.session.limitHint", { count: SESSIONS_LIMIT });
      break;
    case "JOURNAL_LIMIT":
    case "DISK_FULL":
      hint = t("terminal.session.storageFull");
      break;
  }
  return hint ? `${failed} — ${hint} (${error.code})` : errorMessage(error, failed);
}
