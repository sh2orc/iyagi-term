/**
 * Workbench store (04-ui.md §4): SplitNode trees per tab, pane metadata,
 * queue snapshot, and the LATEST host sample only. Output bytes, transcripts,
 * per-keystroke state never enter the store. Selectors are split so terminal
 * renders never re-render the resource strip (U12).
 */

import { create } from "zustand";
import { flushWorkspaceSave, readWorkspace, scheduleWorkspaceSave } from "./workspaceStorage";
import {
  flushWorkloadMemorySave,
  forgetDroppedWorkloads,
  pruneWorkloadMemory,
  readWorkloadMemory,
  rememberWorkloadAgents,
  rememberWorkloadRecovery,
  rememberWorkloadTitle,
  scheduleWorkloadMemorySave,
  type WorkloadMemory,
} from "./workloadMemory";
import { t } from "../i18n";
import type { AgentStatus } from "../generated/AgentStatus";
import type { HostSample } from "../generated/HostSample";
import type { QueueEntry } from "../generated/QueueEntry";
import type { LimitCapability } from "../generated/LimitCapability";
import type { ReliefPolicy } from "../generated/ReliefPolicy";
import type { Snapshot } from "../generated/Snapshot";
import type { WorkloadSummary } from "../generated/WorkloadSummary";
import {
  balanceAllAxisGroups,
  balanceAxisGroup,
  closeLeaf,
  detachLeaf,
  findLeaf,
  joinTrees,
  leafCount,
  listLeaves,
  MAX_PANES_PER_TAB,
  dockLeaf,
  insertBeside,
  nearestLeaf,
  replaceLeaf,
  split,
  swapLeaves,
  type PaneEdge,
  type SplitNode,
} from "../features/terminal/splitTree";
import { applyRegroupPlan, type RegroupPlan } from "../features/terminal/regroup";
import type { LayoutPlacement } from "../features/terminal/layoutDrag";
import { missionProtocolVersion } from "../features/missions/capability";
import type { PanePhase } from "../features/monitor/statusStrings";
import { decodeResumeInfo, type AgentResumeInfo } from "../features/agentSessions/types";
import type { SettingsGroupId } from "../features/settings/schema";
import { isFinishedWorkload } from "./workloadState";

export interface PaneExitInfo {
  code: number | null;
  /** ExitReason enum(계약) — 문구는 이 값에서 생성한다. */
  reason: string;
  /** 기술 근거(예: oom_kill 카운터). 현지화하지 않는다. */
  detail: string | null;
}

export interface PaneMeta {
  leafId: string;
  viewId: string;
  sessionId: string | null;
  workloadId: string | null;
  title: string;
  /**
   * 터미널이 마지막으로 보고한 제목(OSC 0/2). `title`은 재연결 안내·셸
   * 라벨 같은 자리 표시일 수 있어, 관리 작업 목록은 이 값이 있을 때만 쓴다.
   * 저장하지 않는다 — attach 재생이 같은 시퀀스를 다시 보내 복원된다.
   * 새 세션이 배정되면 지운다(이전 세션의 이름을 물려주지 않는다).
   */
  terminalTitle?: string | null;
  /** 이 pane이 사용한 시작 경로(04-ui §2-4 header 표시용). OSC 7로 갱신. */
  cwd: string | null;
  /**
   * cwd가 속한 git 최상위 경로. 헤더의 브랜치 배지가 조회하며 채우고,
   * 저장소 밖이면 null. mission의 저장소 경로(repositoryPath)로 쓰인다 —
   * 재그룹핑(04-ui §2-5)은 이 값이 아니라 cwd로 묶는다. 저장하지 않는다
   * (복원 뒤 다시 조회).
   */
  project?: string | null;
  phase: PanePhase;
  error: string | null;
  /** 최근 usage 요약(header 표시용, 초 단위 갱신). */
  usage: { cpuCores: number | null; residentBytes: number | null } | null;
  flowBlocked: boolean;
  /** 세션 안에서 자동 감지된 AI 코딩 에이전트(claude/codex/opencode). */
  agent?: AgentStatus | null;
  /**
   * 이 pane에 사용자가 직접 고른 배경색("#rrggbb"). null이면 테마/에이전트
   * tint를 따른다(paneBackground.ts). leafId 단위로 작업 공간 저장소에
   * 함께 보존된다(workspaceStorage).
   */
  backgroundColor?: string | null;
  /** 세션 종료 사유(W1-1) — exited 오버레이가 표시. 재시작(starting)에서 초기화. */
  exit?: PaneExitInfo | null;
  /** 저신뢰 패턴 감지 배지(W3-5, opt-in). pane 포커스 시 해제. */
  interventionBadge?: boolean;
  /**
   * 롤링 저널이 지운 앞부분 바이트(02-runner §5). 마지막 attach가 잘린
   * 헤드부터 재생했을 때만 값이 있고, 헤더에 "앞부분 N MiB 지워짐"으로
   * 보인다. 새 attach마다 다시 정한다.
   */
  replayTrimmedBytes?: number | null;
  /**
   * 실행 중 확인한 Claude/Codex 대화. 배지가 사라져도 보존하고 작업공간에
   * 저장해 앱 복원·터미널 재실행에서 같은 ID를 사용한다. 새 셸은 지운다.
   */
  resume?: AgentResumeInfo | null;
  /**
   * 이 세션에 적용된 압력 완화(08-pressure-relief §2). 데몬의
   * `WorkloadSummary.relief` 거울이며, 보내지 않는 구 데몬에서는 NONE이다.
   * 저장하지 않는다 — 데몬 상태라 다음 snapshot이 정한다.
   */
  relief?: PaneRelief;
  /** 사용자가 이 세션을 자동 완화에서 제외했다(08 §0-3). */
  protected?: boolean;
}

/**
 * pane이 보여 주는 완화 상태(08 §2 `ReliefState`의 UI 표현). 계약의
 * `since_ms`는 U64 wire string이라 여기서 한 번만 숫자로 바꾼다(못 읽으면
 * null — 시간은 배지 툴팁에만 쓰인다).
 */
export type PaneRelief =
  | { kind: "NONE" }
  | { kind: "YIELDED"; sinceMs: number | null; manual: boolean; partial: boolean };

/** 완화 정보가 없는 pane의 기본값(구 데몬·아직 첫 snapshot 전). */
export const PANE_RELIEF_NONE: PaneRelief = { kind: "NONE" };

/** 두 완화 상태가 같은가(값 비교 — 초당 오는 snapshot이 헤더를 흔들지 않게). */
export function sameRelief(a: PaneRelief, b: PaneRelief): boolean {
  if (a.kind !== b.kind) return false;
  if (a.kind !== "YIELDED" || b.kind !== "YIELDED") return true;
  return a.sinceMs === b.sinceMs && a.manual === b.manual && a.partial === b.partial;
}

/**
 * 탭 union(05-ui §2). mission/agent-view 탭에는 root가 없다 — `.root` 접근은
 * 반드시 `kind === "terminal"` guard 뒤에 온다. mission 탭에 가짜 PTY/leaf를
 * 만들지 않는다.
 */
export type TabState =
  | { kind: "terminal"; id: string; title: string; root: SplitNode | null }
  | { kind: "mission"; id: string; title: string; missionId: string }
  | { kind: "agent-view"; id: string; title: string; missionId: string; taskId: string };

/** mission 계열 탭(mission + agent-view)의 동시 열림 상한(05 §2). */
export const MAX_MISSION_TABS = 16;

export type ModalState =
  | { kind: "close-panes"; leafIds: string[]; tabId?: string; closeAllTabs?: boolean }
  /** 앱 종료 확인 — `sessions`는 살아 있는 터미널(작업) 수. */
  | { kind: "quit"; sessions: number }
  | { kind: "about"; version: string }
  | { kind: "palette" }
  | { kind: "search" }
  | { kind: "managed-run" }
  | { kind: "shell-select" }
  /** 최근 에이전트 세션 목록(04-ui §5) — 이동·이어서 열기·기록 제거. */
  | { kind: "agent-sessions" }
  /**
   * 끝난 에이전트 대화 여러 개를 한 번에 되살리기 전 확인(대기열 "모두 재개").
   * 한 번에 프로세스 여러 개를 띄우므로 개수를 보이고 확인을 받는다.
   */
  | { kind: "resume-agents"; workloadIds: string[] }
  /** pane을 옮길 대상 탭 고르기(04-ui §2-5). */
  | { kind: "move-pane"; leafId: string }
  /** 탭을 합칠 대상 탭 고르기(04-ui §2-5). */
  | { kind: "merge-tab"; tabId: string }
  /** 프로젝트별 다시 묶기 미리보기 — 계획은 대화상자를 연 시점의 것이다. */
  | { kind: "regroup"; plan: RegroupPlan }
  /**
   * 새 AI 작업 작성 패널 요청. openModal은 이를 missionCreate 상태로 라우팅한다.
   * repositoryPath는 패널을 연 자리의
   * 저장소 경로(초점 pane의 git 최상위 → cwd) — 있으면 입력을 채우고 곧바로 확인한다.
   */
  | { kind: "mission-create"; repositoryPath?: string | null; goal?: string | null; followUpOf?: string | null }
  /** AI 작업 목록(결정 필요·확정 대기·진행 중·끝남·보관됨) — 닫았던 작업 탭을 다시 연다. */
  | { kind: "mission-list" }
  | { kind: "notice"; message: string; detail?: string };

/** 워크벤치 위에 뜨는 전체 화면: 설정, 배치 편집(04-ui §2-6). 뜨는 동안 터미널 화면은 숨기만 한다. */
export type WorkbenchPage = "terminal" | "settings" | "layout";

export interface WorkbenchState {
  tabs: TabState[];
  activeTabId: string | null;
  focusedLeafId: string | null;
  panes: Record<string, PaneMeta>;
  workloads: WorkloadSummary[];
  /**
   * `workloads`의 파생 인덱스 — 스냅샷(≈1 Hz)마다 pane 헤더 셀렉터들이 배열을
   * 선형 스캔하지 않게 `applySnapshot`/`upsertWorkload`에서 함께 유지한다.
   * 세션 중복이 있으면 배열 순서의 **첫** 항목을 고른다(`.find`와 같은 의미).
   */
  workloadById: ReadonlyMap<string, WorkloadSummary>;
  workloadBySession: ReadonlyMap<string, WorkloadSummary>;
  /**
   * 작업(workload id)별로 마지막에 본 터미널 모습: 마지막 제목(OSC 0/2)과
   * 마지막으로 감지한 에이전트. pane의 terminalTitle·agent는 창을 닫거나
   * 에이전트가 끝나면 사라지므로, 닫힌 터미널이 "최근 종료"·"배치 안 됨"
   * 목록에서 실행 제목(zsh) 대신 이 값으로 불리게 여기에 둔다. 작업 목록에서
   * 빠진 작업은 버리고, 앱을 다시 켜도 남게 저장한다(workloadMemory.ts).
   */
  workloadMemory: WorkloadMemory;
  queue: QueueEntry[];
  host: HostSample | null;
  revision: number;
  /** capabilities.platform — 문구 생성용 enum 데이터(04 §7). */
  daemonPlatform: string | null;
  /**
   * 사용자 홈(Tauri homeDir) — 경로 표시를 `~`로 줄이는 데만 쓴다(displayPath.ts).
   * 브라우저·시험 환경처럼 모르면 null이고, 그때는 원문 경로를 보인다.
   */
  homeDir: string | null;
  /** capabilities.mission_protocol 미러(O01) — null이면 mission UI 잠금. */
  missionProtocol: number | null;
  /**
   * capabilities.claude_provider_routing 미러 — 데몬이 LaunchRequest.claude_provider
   * (Claude Code의 Z.ai 라우팅)를 해석할 수 있는가. 구 데몬은 이 필드를 보내지
   * 않으며 그때는 false다: 라우팅 설정이 켜져 있어도 조용히 Anthropic으로
   * 떨어지지 않고 실행을 거절한다(features/workloads/claudeProvider.ts).
   */
  claudeProviderRouting: boolean;
  /**
   * capabilities.scheduling_yield 미러(08 §2). 되돌릴 수 없는 플랫폼은
   * unsupported이고 그때 완화 메뉴·정책 토글은 사유를 달아 잠근다.
   * 첫 snapshot 전에는 null(아직 모른다).
   */
  schedulingYield: LimitCapability | null;
  /** 데몬의 현재 완화 정책(08 §2 `relief.auto_yield`). */
  reliefPolicy: ReliefPolicy;
  queueDrawerOpen: boolean;
  graphDrawerOpen: boolean;
  modal: ModalState | null;
  /** Non-modal task composer alongside the terminal group. */
  missionCreate: Extract<ModalState, { kind: "mission-create" }> | null;
  closeMissionCreate(): void;
  page: WorkbenchPage;
  /** 설정 페이지가 열릴 때 먼저 보여줄 그룹(관리 실행 진입점 → "run"). 페이지를 떠나면 null. */
  settingsGroup: SettingsGroupId | null;
  toast: string | null;
  /** 인라인 이름 편집 중인 탭(없으면 null) — chrome 상태다(04 §4). */
  renamingTabId: string | null;
  /** 동기 입력: 켜면 같은 탭의 모든 pane이 같은 입력을 받는다(기본 off). */
  broadcastInput: boolean;
  /**
   * 사용자가 닫아 숨긴 mission id(05 §2·§8). 탭 닫기는 실행을 취소하지
   * 않으므로 "숨김"만 기록한다. UI 로컬 상태라 workspace에 저장하지 않는다.
   */
  hiddenMissions: ReadonlySet<string>;

  // ---- tree/pane actions
  addTab(tabId: string, title: string): void;
  closeTab(tabId: string): void;
  /**
   * mission 탭 열기(05 §2): 같은 mission이면 기존 탭으로 이동하고, 새 탭은
   * mission 계열 상한까지만 만든다. 초과해도 실행을 취소하지 않고 toast로
   * 안내한다. 반환값은 포커스된 탭 id(거부되면 null).
   */
  openMissionTab(missionId: string, title: string): string | null;
  /** agent-view 탭 열기 — task ID로 중복 검사(관찰만, 새 실행이 아니다). */
  openAgentViewTab(missionId: string, taskId: string, title: string): string | null;
  setActiveTab(tabId: string): void;
  /**
   * 탭 순환(트랙패드 가로 스와이프·Cmd+Shift+[ ·] ): 지금 탭에서 delta만큼
   * 옆으로 옮긴다. `wrap`이면 끝에서 반대편 끝으로 돌고, 아니면 끝에서 멈춘다.
   * 전환은 setActiveTab을 거치므로 초점 복원은 탭을 클릭한 것과 같다.
   * 실제로 활성 탭이 바뀌었으면 true(탭이 2개 미만이면 언제나 false).
   */
  cycleTab(delta: 1 | -1, wrap: boolean): boolean;
  /** 탭 이름 인라인 편집 시작(null이면 편집 종료). */
  startTabRename(tabId: string | null): void;
  /** 사용자가 지은 탭 이름. 공백뿐이면 거절해 기존 이름을 지킨다. */
  renameTab(tabId: string, title: string): void;
  focusPane(leafId: string): void;
  applySplit(
    tabId: string,
    focusedLeafId: string,
    newPane: { leafId: string; viewId: string; sessionId: string | null; title: string; cwd: string | null },
    splitId: string,
    axis: "row" | "column",
  ): boolean;
  applyClose(leafId: string): void;
  setRatio(tabId: string, splitId: string, ratio: number): void;
  /** divider 더블클릭: 해당 축 그룹 비율을 균등으로 되돌린다. */
  balanceRatios(tabId: string, splitId: string): void;

  // ---- 재배치(04-ui §2-5): 세션·뷰는 그대로, 트리만 옮긴다.
  /** 탭 순서 이동(인덱스는 범위로 자른다). 자리가 같으면 상태를 바꾸지 않는다. */
  moveTab(tabId: string, toIndex: number): void;
  /**
   * pane을 다른 탭으로 옮긴다: 대상 탭의 초점 pane(없으면 마지막 pane) 옆에
   * 좌우 분할로 끼우고 그 축 그룹을 균등화한다. 원래 탭이 비면 닫는다.
   * 옮긴 pane에 초점·활성 탭이 따라간다. 상한(탭당 8)·같은 탭·모르는 id면
   * false.
   */
  movePaneToTab(leafId: string, targetTabId: string, splitId: string): boolean;
  /**
   * pane을 새 탭으로 떼어 낸다(원래 탭에 다른 pane이 있을 때만 — 혼자면 이미 탭이다).
   * 새 탭은 원래 탭 바로 오른쪽에 두고, atIndex를 주면(끌어 놓기) 그 틈에 끼운다.
   */
  detachPaneToNewTab(leafId: string, newTabId: string, title: string, atIndex?: number): boolean;
  /**
   * source 탭의 pane 전부를 target 탭에 합친다: 두 트리를 좌우 split 하나로
   * 잇는다(각 탭 안의 배치·비율은 그대로). 합이 상한을 넘으면 false.
   */
  mergeTabs(sourceTabId: string, targetTabId: string, splitId: string): boolean;
  /**
   * 끌어 놓기: pane을 다른 pane의 가장자리(edge) 옆으로 옮긴다. 같은 탭이면 그 트리
   * 안에서(이미 그 자리면 트리는 그대로), 다른 탭이면 떼어 붙인다 — 상한(탭당 8)·
   * 비어 버린 원래 탭 닫기는 movePaneToTab과 같다. 초점·활성 탭은 옮긴 pane을 따라간다.
   */
  dockPane(leafId: string, targetLeafId: string, edge: PaneEdge, splitId: string): boolean;
  /** 끌어 놓기: 두 pane의 자리를 맞바꾼다(같은 탭·다른 탭). 초점·활성 탭은 끌어 온 pane을 따라간다. */
  swapPanes(leafId: string, targetLeafId: string): boolean;
  /**
   * 배치 편집: 이미 실행 중인 세션을 붙일 새 pane을 자리(placement)에 만든다 — 그 탭 안(초점 pane
   * 옆, 없으면 마지막 pane 오른쪽), 어느 pane의 가장자리 옆, 또는 틈의 새 탭. 상한(탭당 8)·모르는
   * 자리·이미 있는 id면 null, 성공하면 pane이 들어간 탭 id. 초점·활성 탭은 새 pane을 따라간다.
   */
  placeNewPane(
    placement: LayoutPlacement,
    pane: {
      leafId: string;
      viewId: string;
      sessionId: string;
      workloadId: string | null;
      title: string;
      cwd: string | null;
      phase: PanePhase;
    },
    ids: { splitId: string; tabId: string; tabTitle: string },
  ): string | null;
  /** 재그룹핑 계획 적용(regroup.applyRegroupPlan). 계획이 현재 상태와 안 맞으면 false. */
  applyRegroup(plan: RegroupPlan, makeId: () => string): boolean;
  paneSessionAssigned(leafId: string, sessionId: string, workloadId: string): void;
  patchLeaf(leafId: string, patch: { session_id?: string | null; view_id?: string }): void;
  panePhase(leafId: string, phase: PanePhase, error?: string | null): void;
  paneUsage(leafId: string, usage: PaneMeta["usage"]): void;
  /** 감지된 에이전트 갱신(자동 감지 베이스 — WorkloadSummary.agent 반영). */
  paneAgent(leafId: string, agent: AgentStatus | null): void;
  /** pane 전용 배경색 지정/해제(null이면 테마·에이전트 tint). */
  paneBackgroundColor(leafId: string, color: string | null): void;
  paneFlowBlocked(leafId: string, blocked: boolean): void;
  /** 잘린 헤드부터 재생했음을 기록(null이면 지움). */
  paneReplayTrimmed(leafId: string, bytes: number | null): void;
  paneTitle(leafId: string, title: string, cwd: string | null): void;
  /** 터미널이 보고한 동적 제목(OSC 0/2) — pane 헤더와 관리 작업 목록이 함께 따른다. */
  paneTerminalTitle(leafId: string, title: string): void;
  /** 끝난 작업을 새 작업이 이어받았다(복구·재실행) — 최근 종료 목록에서 뺀다. */
  markWorkloadRecovered(fromWorkloadId: string, toWorkloadId: string): void;
  /** OSC 7로 갱신된 실제 cwd(헤더·분할 상속이 참값을 쓴다 — W1-4). */
  paneCwd(leafId: string, cwd: string): void;
  /** cwd가 속한 git 최상위 경로(없으면 null). 같은 값이면 상태를 바꾸지 않는다. */
  paneProject(leafId: string, project: string | null): void;
  /** 세션 종료 사유 기록(W1-1). 재시작(starting)에서는 초기화한다. */
  paneExit(leafId: string, exit: PaneExitInfo): void;
  /** 사용자 홈을 알려 준다 — Workbench가 마운트 때 한 번 넣는다. */
  setHomeDir(home: string | null): void;
  /** 패턴 감지 배지(W3-5). 사용자가 pane을 보면 끈다. */
  paneInterventionBadge(leafId: string, on: boolean): void;
  /**
   * 완화 상태 거울(08 §2). 데몬 요약·수동 액션 결과를 그대로 받아 적고,
   * 값이 같으면 상태를 바꾸지 않는다(초당 오는 snapshot이 헤더를 다시
   * 그리지 않게).
   */
  paneRelief(leafId: string, relief: PaneRelief, isProtected: boolean): void;
  /** 이어서 열 수 있는 에이전트 세션 기록(04-ui §5). null이면 지운다. */
  paneResume(leafId: string, resume: AgentResumeInfo | null): void;
  removePaneMeta(leafId: string): void;

  // ---- daemon mirror
  setQueue(queue: QueueEntry[]): void;
  upsertWorkload(workload: WorkloadSummary): void;
  setHostSample(sample: HostSample): void;
  applySnapshot(snapshot: Snapshot): void;
  /** `relief.set_policy` 응답을 그대로 반영한다(08 §2 — 데몬이 적용한 값). */
  setReliefPolicy(policy: ReliefPolicy): void;

  // ---- chrome
  openModal(modal: ModalState): void;
  closeModal(): void;
  setPage(page: WorkbenchPage): void;
  /** 설정 페이지를 그 그룹으로 연다(메뉴 막대의 설정…·키보드 단축키). 모달·탭 이름 편집은 닫는다. */
  openSettings(group: SettingsGroupId): void;
  setToast(message: string | null): void;
  toggleQueueDrawer(open?: boolean): void;
  setGraphDrawer(open: boolean): void;
  toggleBroadcastInput(on?: boolean): void;
}

export const useWorkbenchStore = create<WorkbenchState>((set, get) => ({
  tabs: [],
  activeTabId: null,
  focusedLeafId: null,
  panes: {},
  workloads: [],
  workloadById: new Map(),
  workloadBySession: new Map(),
  workloadMemory: readWorkloadMemory(),
  queue: [],
  host: null,
  revision: 0,
  daemonPlatform: null,
  homeDir: null,
  missionProtocol: null,
  claudeProviderRouting: false,
  schedulingYield: null,
  // 데몬 기본값(defaults.json relief.auto_yield)과 같게 시작한다 — 첫
  // snapshot이 도착하면 그 값이 정본이다.
  reliefPolicy: { auto_yield: true },
  queueDrawerOpen: false,
  graphDrawerOpen: false,
  modal: null,
  missionCreate: null,
  closeMissionCreate: () => set({ missionCreate: null }),
  page: "terminal",
  settingsGroup: null,
  toast: null,
  renamingTabId: null,
  broadcastInput: false,
  hiddenMissions: new Set<string>(),

  ...readWorkspace(),

  addTab: (tabId, title) =>
    set((s) =>
      s.tabs.some((t) => t.id === tabId)
        ? s
        : { tabs: [...s.tabs, { kind: "terminal", id: tabId, title, root: null }], activeTabId: s.activeTabId ?? tabId },
    ),

  closeTab: (tabId) =>
    set((s) => {
      const tab = s.tabs.find((t) => t.id === tabId);
      if (!tab) return s;
      const tabs = s.tabs.filter((t) => t.id !== tabId);
      const activeTabId = s.activeTabId === tabId ? (tabs[0]?.id ?? null) : s.activeTabId;
      if (tab.kind !== "terminal") {
        // mission 탭 닫기 = 로컬 숨김(05 §8). 실행은 daemon에 그대로 있고
        // agent-view는 관찰 뷰일 뿐이므로 어느 쪽도 취소 요청을 만들지 않는다.
        return {
          tabs,
          activeTabId,
          focusedLeafId: focusWithinTab(tabs, activeTabId, s.focusedLeafId),
          hiddenMissions:
            tab.kind === "mission" ? new Set(s.hiddenMissions).add(tab.missionId) : s.hiddenMissions,
          renamingTabId: s.renamingTabId === tabId ? null : s.renamingTabId,
        };
      }
      const panes = { ...s.panes };
      const removed: string[] = [];
      const walk = (node: SplitNode | null) => {
        if (!node) return;
        if (node.kind === "leaf") {
          delete panes[node.id];
          removed.push(node.id);
        } else {
          walk(node.first);
          walk(node.second);
        }
      };
      walk(tab.root);
      const focused = removed.includes(s.focusedLeafId ?? "") ? null : s.focusedLeafId;
      return {
        tabs,
        panes,
        activeTabId,
        focusedLeafId: focusWithinTab(tabs, activeTabId, focused),
        renamingTabId: s.renamingTabId === tabId ? null : s.renamingTabId,
      };
    }),

  openMissionTab: (missionId, title) => {
    let focusedTabId: string | null = null;
    set((s) => {
      const existing = s.tabs.find((t) => t.kind === "mission" && t.missionId === missionId);
      if (existing) {
        // 같은 mission은 한 창에 탭 하나(05 §2) — 새 탭 대신 기존 탭으로 이동.
        focusedTabId = existing.id;
        return {
          ...showTabsPage(s),
          activeTabId: existing.id,
          focusedLeafId: null,
          hiddenMissions: removeFromSet(s.hiddenMissions, missionId),
        };
      }
      if (s.tabs.filter((t) => t.kind !== "terminal").length >= MAX_MISSION_TABS) {
        // 상한 초과: mission 생성/실행을 취소하지 않고 안내만(05 §2).
        return { toast: t("missions.tabLimit", { count: MAX_MISSION_TABS }) };
      }
      const tabId = newTabId();
      focusedTabId = tabId;
      return {
        ...showTabsPage(s),
        tabs: [...s.tabs, { kind: "mission", id: tabId, title, missionId }],
        activeTabId: tabId,
        focusedLeafId: null,
        hiddenMissions: removeFromSet(s.hiddenMissions, missionId),
      };
    });
    return focusedTabId;
  },

  openAgentViewTab: (missionId, taskId, title) => {
    let focusedTabId: string | null = null;
    set((s) => {
      const existing = s.tabs.find((t) => t.kind === "agent-view" && t.taskId === taskId);
      if (existing) {
        focusedTabId = existing.id;
        return { ...showTabsPage(s), activeTabId: existing.id, focusedLeafId: null };
      }
      if (s.tabs.filter((t) => t.kind !== "terminal").length >= MAX_MISSION_TABS) {
        return { toast: t("missions.tabLimit", { count: MAX_MISSION_TABS }) };
      }
      const tabId = newTabId();
      focusedTabId = tabId;
      return {
        ...showTabsPage(s),
        tabs: [...s.tabs, { kind: "agent-view", id: tabId, title, missionId, taskId }],
        activeTabId: tabId,
        focusedLeafId: null,
      };
    });
    return focusedTabId;
  },

  setActiveTab: (tabId) =>
    set((s) => {
      if (!s.tabs.some((t) => t.id === tabId)) return s;
      // 단축키(붙여넣기·닫기·확대·분할)는 focusedLeafId를 따른다 — 이전 탭의
      // 숨은 pane을 계속 가리키면 입력이 보이지 않는 셸로 간다.
      const focusedLeafId = focusWithinTab(s.tabs, tabId, s.focusedLeafId);
      if (s.activeTabId === tabId && focusedLeafId === s.focusedLeafId) return s;
      return { activeTabId: tabId, focusedLeafId };
    }),

  cycleTab: (delta, wrap) => {
    const s = get();
    // 탭이 하나뿐(또는 없음)이면 옮길 곳이 없다 — 제스처는 조용히 무시된다.
    if (s.tabs.length < 2) return false;
    const from = s.tabs.findIndex((t) => t.id === s.activeTabId);
    if (from < 0) return false;
    const raw = from + delta;
    const to = wrap ? (raw + s.tabs.length) % s.tabs.length : raw;
    if (to < 0 || to >= s.tabs.length) return false;
    const target = s.tabs[to];
    if (target.id === s.activeTabId) return false;
    // 클릭과 같은 경로 — 초점 pane 복원 규칙을 한 곳(setActiveTab)에만 둔다.
    get().setActiveTab(target.id);
    return get().activeTabId === target.id;
  },

  startTabRename: (tabId) =>
    set((s) => (tabId === null || s.tabs.some((t) => t.id === tabId) ? { renamingTabId: tabId } : s)),

  renameTab: (tabId, title) =>
    set((s) => {
      // 앞뒤 공백은 버리고, 공백뿐인 이름은 거절한다(이름 없는 탭 금지).
      // 이름 편집은 terminal 탭 전용(05 §2) — mission 제목은 missionStore가 정본.
      const next = title.trim();
      const tab = s.tabs.find((t) => t.id === tabId);
      if (next.length === 0 || !tab || tab.kind !== "terminal" || tab.title === next) return s;
      return { tabs: s.tabs.map((t) => (t.id === tabId && t.kind === "terminal" ? { ...t, title: next } : t)) };
    }),

  focusPane: (leafId) => set({ focusedLeafId: leafId }),

  applySplit: (tabId, focusedLeafId, newPane, splitId, axis) => {
    const s = get();
    const tab = s.tabs.find((t) => t.id === tabId);
    if (!tab || tab.kind !== "terminal") return false;
    const root = tab.root;
    if (!root) return false;
    const nextRoot = split(
      root,
      focusedLeafId,
      { id: newPane.leafId, view_id: newPane.viewId, session_id: newPane.sessionId },
      { splitId, axis },
    );
    if (!nextRoot) return false;
    // 새 split이 속한 축 그룹을 균등 비율로 재조정한다: 50:50 → 1/3씩 →
    // 1/4씩 (04-ui.md §2-3). 다른 축 그룹의 비율은 그대로 둔다.
    const balancedRoot = balanceAxisGroup(nextRoot, splitId);
    set({
      tabs: s.tabs.map((t) => (t.id === tabId && t.kind === "terminal" ? { ...t, root: balancedRoot } : t)),
      panes: {
        ...s.panes,
        [newPane.leafId]: {
          leafId: newPane.leafId,
          viewId: newPane.viewId,
          sessionId: newPane.sessionId,
          workloadId: null,
          title: newPane.title,
          cwd: newPane.cwd,
          phase: "starting",
          error: null,
          usage: null,
          flowBlocked: false,
          agent: null,
          relief: PANE_RELIEF_NONE,
          protected: false,
        },
      },
      focusedLeafId: newPane.leafId,
    });
    return true;
  },

  applyClose: (leafId) => {
    const s = get();
    for (const tab of s.tabs) {
      if (tab.kind !== "terminal" || !tab.root) continue;
      const result = closeLeaf(tab.root, leafId);
      if (result.root === tab.root && result.focusLeafId === null) continue; // not in this tab
      const balancedRoot = result.root ? balanceAllAxisGroups(result.root) : null;
      const panes = { ...s.panes };
      delete panes[leafId];
      set({
        tabs: s.tabs.map((t) => (t.id === tab.id && t.kind === "terminal" ? { ...t, root: balancedRoot } : t)),
        panes,
        // 초점은 닫은 창이 초점이었을 때만 옆 창으로 옮긴다 — 배경 탭·지우는 그룹의 창을 닫았다고
        // 보고 있는 창의 초점이 바뀌면 입력이 엉뚱한 셸로 간다.
        focusedLeafId: s.focusedLeafId === leafId ? result.focusLeafId : s.focusedLeafId,
      });
      return;
    }
  },

  setRatio: (tabId, splitId, ratio) =>
    set((s) => ({
      tabs: s.tabs.map((t) =>
        t.id === tabId && t.kind === "terminal" && t.root ? { ...t, root: withRatioMut(t.root, splitId, ratio) } : t,
      ),
    })),

  balanceRatios: (tabId, splitId) =>
    set((s) => {
      const tab = s.tabs.find((t) => t.id === tabId);
      if (!tab || tab.kind !== "terminal" || !tab.root) return s;
      const root = balanceAxisGroup(tab.root, splitId);
      // 이미 균등하면 같은 참조가 돌아온다 → 상태를 바꾸지 않는다.
      if (root === tab.root) return s;
      return { tabs: s.tabs.map((t) => (t.id === tabId && t.kind === "terminal" ? { ...t, root } : t)) };
    }),

  moveTab: (tabId, toIndex) =>
    set((s) => {
      const from = s.tabs.findIndex((t) => t.id === tabId);
      if (from < 0) return s;
      const to = Math.max(0, Math.min(s.tabs.length - 1, toIndex));
      if (to === from) return s;
      const tabs = [...s.tabs];
      const [tab] = tabs.splice(from, 1);
      tabs.splice(to, 0, tab);
      return { tabs };
    }),

  movePaneToTab: (leafId, targetTabId, splitId) => {
    const s = get();
    const source = s.tabs.find((t) => t.kind === "terminal" && findLeaf(t.root, leafId) !== null);
    const target = s.tabs.find((t) => t.id === targetTabId);
    if (!source || !target || source.id === target.id) return false;
    // pane은 terminal 탭 사이에서만 옮긴다 — mission/agent-view 탭에는 트리가 없다(05 §2).
    if (source.kind !== "terminal" || target.kind !== "terminal") return false;
    if (leafCount(target.root) >= MAX_PANES_PER_TAB) return false;
    const detached = detachLeaf(source.root, leafId);
    const leaf = detached.leaf;
    if (!leaf || leaf.kind !== "leaf") return false;
    let targetRoot: SplitNode;
    if (!target.root) {
      targetRoot = leaf;
    } else {
      // 대상 탭 안의 초점 pane 옆, 아니면 마지막 pane 옆.
      const anchor =
        s.focusedLeafId && findLeaf(target.root, s.focusedLeafId) ? s.focusedLeafId : nearestLeaf(target.root, "end");
      const next = split(
        target.root,
        anchor,
        { id: leaf.id, view_id: leaf.view_id, session_id: leaf.session_id },
        { splitId, axis: "row" },
      );
      if (!next) return false;
      targetRoot = balanceAxisGroup(next, splitId);
    }
    const sourceRoot = detached.root ? balanceAllAxisGroups(detached.root) : null;
    const tabs = s.tabs
      .map((t) =>
        t.kind !== "terminal"
          ? t
          : t.id === target.id
            ? { ...t, root: targetRoot }
            : t.id === source.id
              ? { ...t, root: sourceRoot }
              : t,
      )
      // 비어 버린 원래 탭은 닫는다 — 옮기기의 목적은 정리다.
      .filter((t) => !(t.id === source.id && sourceRoot === null));
    set({
      tabs,
      activeTabId: target.id,
      focusedLeafId: leaf.id,
      renamingTabId: sourceRoot === null && s.renamingTabId === source.id ? null : s.renamingTabId,
    });
    return true;
  },

  detachPaneToNewTab: (leafId, newTabId, title, atIndex) => {
    const s = get();
    const source = s.tabs.find((t) => t.kind === "terminal" && findLeaf(t.root, leafId) !== null);
    if (!source || source.kind !== "terminal" || s.tabs.some((t) => t.id === newTabId)) return false;
    // 혼자 있는 pane은 이미 자기 탭이다 — 새 탭을 만들 이유가 없다.
    if (leafCount(source.root) <= 1) return false;
    const detached = detachLeaf(source.root, leafId);
    if (!detached.leaf || !detached.root) return false;
    const remaining = balanceAllAxisGroups(detached.root);
    const sourceIndex = s.tabs.findIndex((t) => t.id === source.id);
    const tabs: TabState[] = s.tabs.map((t) =>
      t.id === source.id && t.kind === "terminal" ? { ...t, root: remaining } : t,
    );
    // 새 탭은 원래 탭 바로 오른쪽에 — 어디서 나왔는지 눈으로 따라갈 수 있게.
    // 끌어 놓기는 놓은 틈(atIndex — 지금 탭 배열 기준)에 끼운다.
    const insertAt =
      atIndex === undefined ? sourceIndex + 1 : Math.max(0, Math.min(tabs.length, Math.trunc(atIndex)));
    tabs.splice(insertAt, 0, { kind: "terminal", id: newTabId, title, root: detached.leaf });
    set({ tabs, activeTabId: newTabId, focusedLeafId: leafId });
    return true;
  },

  mergeTabs: (sourceTabId, targetTabId, splitId) => {
    const s = get();
    const source = s.tabs.find((t) => t.id === sourceTabId);
    const target = s.tabs.find((t) => t.id === targetTabId);
    if (!source || !target || source.id === target.id) return false;
    // 합치기는 terminal 탭끼리만 — mission/agent-view 탭에는 합칠 트리가 없다(05 §2).
    if (source.kind !== "terminal" || target.kind !== "terminal") return false;
    if (!source.root) {
      // 빈 탭을 합치는 것은 그 탭을 닫는 것과 같다.
      const activeTabId = s.activeTabId === source.id ? target.id : s.activeTabId;
      set({
        tabs: s.tabs.filter((t) => t.id !== source.id),
        activeTabId,
        focusedLeafId: focusWithinTab(s.tabs, activeTabId, s.focusedLeafId),
        renamingTabId: s.renamingTabId === source.id ? null : s.renamingTabId,
      });
      return true;
    }
    if (leafCount(source.root) + leafCount(target.root) > MAX_PANES_PER_TAB) return false;
    const root = target.root ? joinTrees(target.root, source.root, splitId, "row") : source.root;
    // 초점은 옮겨 온 쪽의 첫 pane — 방금 합친 것이 어디 붙었는지 보인다.
    const focusedLeafId = nearestLeaf(source.root, "start");
    set({
      tabs: s.tabs
        .filter((t) => t.id !== source.id)
        .map((t) => (t.id === target.id && t.kind === "terminal" ? { ...t, root } : t)),
      activeTabId: target.id,
      focusedLeafId,
      renamingTabId: s.renamingTabId === source.id ? null : s.renamingTabId,
    });
    return true;
  },

  dockPane: (leafId, targetLeafId, edge, splitId) => {
    const s = get();
    if (leafId === targetLeafId) return false;
    const source = terminalTabOf(s.tabs, leafId);
    const target = terminalTabOf(s.tabs, targetLeafId);
    if (!source?.root || !target?.root) return false;
    if (source.id === target.id) {
      const root = dockLeaf(source.root, leafId, targetLeafId, edge, splitId);
      if (!root) return false;
      // 이미 그 자리(같은 모양)면 트리는 그대로 두고 초점만 맞춘다 — 조정해 둔 비율을 지킨다.
      set({
        tabs: root === source.root ? s.tabs : withRoots(s.tabs, new Map([[source.id, root]])),
        activeTabId: source.id,
        focusedLeafId: leafId,
      });
      return true;
    }
    if (leafCount(target.root) >= MAX_PANES_PER_TAB) return false;
    const detached = detachLeaf(source.root, leafId);
    if (!detached.leaf) return false;
    const targetRoot = insertBeside(target.root, targetLeafId, detached.leaf, edge, splitId);
    if (!targetRoot) return false;
    const sourceRoot = detached.root ? balanceAllAxisGroups(detached.root) : null;
    set({
      tabs: withRoots(
        s.tabs,
        new Map<string, SplitNode | null>([
          [source.id, sourceRoot],
          [target.id, targetRoot],
        ]),
      ),
      activeTabId: target.id,
      focusedLeafId: leafId,
      renamingTabId: sourceRoot === null && s.renamingTabId === source.id ? null : s.renamingTabId,
    });
    return true;
  },

  swapPanes: (leafId, targetLeafId) => {
    const s = get();
    if (leafId === targetLeafId) return false;
    const source = terminalTabOf(s.tabs, leafId);
    const target = terminalTabOf(s.tabs, targetLeafId);
    if (!source?.root || !target?.root) return false;
    const roots = new Map<string, SplitNode | null>();
    if (source.id === target.id) {
      const root = swapLeaves(source.root, leafId, targetLeafId);
      if (!root) return false;
      roots.set(source.id, root);
    } else {
      // 탭 사이 자리 바꾸기: 두 leaf 객체를 서로의 자리에 끼운다(창 수는 그대로라 상한과 무관).
      const moved = findLeaf(source.root, leafId);
      const other = findLeaf(target.root, targetLeafId);
      const sourceRoot = moved && other ? replaceLeaf(source.root, leafId, other) : null;
      const targetRoot = moved && other ? replaceLeaf(target.root, targetLeafId, moved) : null;
      if (!sourceRoot || !targetRoot) return false;
      roots.set(source.id, sourceRoot).set(target.id, targetRoot);
    }
    set({ tabs: withRoots(s.tabs, roots), activeTabId: target.id, focusedLeafId: leafId });
    return true;
  },

  placeNewPane: (placement, pane, ids) => {
    const s = get();
    if (s.panes[pane.leafId] || terminalTabOf(s.tabs, pane.leafId)) return null;
    const leaf: SplitNode = { kind: "leaf", id: pane.leafId, view_id: pane.viewId, session_id: pane.sessionId };
    let tabs: TabState[];
    let tabId: string;
    if (placement.kind === "new-tab") {
      if (s.tabs.some((t) => t.id === ids.tabId)) return null;
      tabs = [...s.tabs];
      const at = Math.max(0, Math.min(tabs.length, Math.trunc(placement.atIndex)));
      tabs.splice(at, 0, { kind: "terminal", id: ids.tabId, title: ids.tabTitle, root: leaf });
      tabId = ids.tabId;
    } else {
      const target =
        placement.kind === "tab"
          ? (s.tabs.find((t) => t.id === placement.tabId) ?? null)
          : terminalTabOf(s.tabs, placement.leafId);
      if (!target || target.kind !== "terminal" || leafCount(target.root) >= MAX_PANES_PER_TAB) return null;
      let root: SplitNode | null = null;
      if (!target.root) {
        root = placement.kind === "tab" ? leaf : null;
      } else if (placement.kind === "tab") {
        // 그 탭 안의 초점 pane 옆, 아니면 마지막 pane 오른쪽 — 다른 탭으로 이동과 같은 자리다.
        const anchor =
          s.focusedLeafId && findLeaf(target.root, s.focusedLeafId) ? s.focusedLeafId : nearestLeaf(target.root, "end");
        root = insertBeside(target.root, anchor, leaf, "right", ids.splitId);
      } else {
        root = insertBeside(target.root, placement.leafId, leaf, placement.edge, ids.splitId);
      }
      if (!root) return null;
      tabs = withRoots(s.tabs, new Map([[target.id, root]]));
      tabId = target.id;
    }
    set({
      tabs,
      panes: {
        ...s.panes,
        [pane.leafId]: {
          leafId: pane.leafId,
          viewId: pane.viewId,
          sessionId: pane.sessionId,
          workloadId: pane.workloadId,
          title: pane.title,
          cwd: pane.cwd,
          phase: pane.phase,
          error: null,
          usage: null,
          flowBlocked: false,
          agent: null,
          relief: PANE_RELIEF_NONE,
          protected: false,
        },
      },
      activeTabId: tabId,
      focusedLeafId: pane.leafId,
    });
    return tabId;
  },

  applyRegroup: (plan, makeId) => {
    const s = get();
    const result = applyRegroupPlan(
      { tabs: s.tabs, panes: s.panes, activeTabId: s.activeTabId, focusedLeafId: s.focusedLeafId },
      plan,
      makeId,
    );
    if (!result) return false;
    // 모든 leaf와 pane 없는 탭(mission 계열)이 살아남았는지 한 번 더 확인한다 —
    // pane 메타가 고아가 되거나 mission 탭이 사라지면 안 된다.
    const survivors = (tabs: TabState[]): Set<string> =>
      new Set(
        tabs.flatMap((t) => (t.kind === "terminal" ? listLeaves(t.root).map((leaf) => `leaf:${leaf.id}`) : [`tab:${t.id}`])),
      );
    const before = survivors(s.tabs);
    const after = survivors(result.tabs);
    if (before.size !== after.size || [...before].some((id) => !after.has(id))) return false;
    set({ ...result, renamingTabId: null });
    return true;
  },

  paneSessionAssigned: (leafId, sessionId, workloadId) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane) return s;
      // SplitNode leaf의 session_id도 함께 갱신(트리가 정본이다 — 04 §2).
      const tabs = s.tabs.map((t) =>
        t.kind === "terminal" && t.root ? { ...t, root: patchLeafIn(t.root, leafId, { session_id: sessionId }) } : t,
      );
      return {
        tabs,
        panes: {
          ...s.panes,
          [leafId]: {
            ...pane,
            sessionId,
            workloadId,
            phase: "replaying",
            // 새 세션은 이전 세션이 보고한 제목을 이름으로 물려받지 않는다.
            terminalTitle: pane.sessionId === sessionId ? pane.terminalTitle : null,
          },
        },
      };
    }),

  patchLeaf: (leafId, patch) =>
    set((s) => ({
      tabs: s.tabs.map((t) => (t.kind === "terminal" && t.root ? { ...t, root: patchLeafIn(t.root, leafId, patch) } : t)),
    })),

  panePhase: (leafId, phase, error = null) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane) return s;
      // 새 시도(starting)는 이전 종료 사유를 남기지 않는다.
      const exit = phase === "starting" ? null : pane.exit;
      if (pane.phase === phase && pane.error === error && exit === pane.exit) return s;
      return { panes: { ...s.panes, [leafId]: { ...pane, phase, error, exit } } };
    }),

  paneUsage: (leafId, usage) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane) return s;
      return { panes: { ...s.panes, [leafId]: { ...pane, usage } } };
    }),

  paneAgent: (leafId, agent) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane || pane.agent === agent) return s;
      const sameSession = agent?.agent === pane.resume?.agent && agent?.session_id === pane.resume?.agentSessionId;
      const resume = sameSession ? pane.resume : decodeResumeInfo({
        agent: agent?.agent,
        agentSessionId: agent?.session_id,
        cwd: pane.cwd,
        title: agent?.session_name,
      }) ?? pane.resume;
      return { panes: { ...s.panes, [leafId]: { ...pane, agent, resume } } };
    }),

  paneBackgroundColor: (leafId, color) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane || (pane.backgroundColor ?? null) === color) return s;
      return { panes: { ...s.panes, [leafId]: { ...pane, backgroundColor: color } } };
    }),

  paneFlowBlocked: (leafId, blocked) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane || pane.flowBlocked === blocked) return s;
      return { panes: { ...s.panes, [leafId]: { ...pane, flowBlocked: blocked } } };
    }),

  paneReplayTrimmed: (leafId, bytes) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane || (pane.replayTrimmedBytes ?? null) === bytes) return s;
      return { panes: { ...s.panes, [leafId]: { ...pane, replayTrimmedBytes: bytes } } };
    }),

  paneTitle: (leafId, title, cwd) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane) return s;
      return { panes: { ...s.panes, [leafId]: { ...pane, title, cwd: cwd ?? pane.cwd } } };
    }),

  paneTerminalTitle: (leafId, title) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane) return s;
      // 창을 닫아도 남도록 작업 단위로도 기억한다. 작업 id 없이 세션만 붙인
      // pane(재연결)은 세션으로 작업을 찾는다.
      const workloadId =
        pane.workloadId ??
        (pane.sessionId === null ? null : s.workloadBySession.get(pane.sessionId)?.workload_id ?? null);
      const workloadMemory =
        workloadId === null ? s.workloadMemory : rememberWorkloadTitle(s.workloadMemory, workloadId, title);
      // attach 재생은 같은 OSC를 다시 보낸다 — 같은 값이면 구독자를 깨우지 않는다.
      const paneChanged = pane.title !== title || pane.terminalTitle !== title;
      if (!paneChanged && workloadMemory === s.workloadMemory) return s;
      return {
        panes: paneChanged ? { ...s.panes, [leafId]: { ...pane, title, terminalTitle: title } } : s.panes,
        workloadMemory,
      };
    }),

  markWorkloadRecovered: (fromWorkloadId, toWorkloadId) =>
    set((s) => {
      const workloadMemory = rememberWorkloadRecovery(s.workloadMemory, fromWorkloadId, toWorkloadId);
      return workloadMemory === s.workloadMemory ? s : { workloadMemory };
    }),

  paneCwd: (leafId, cwd) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane || pane.cwd === cwd) return s;
      return { panes: { ...s.panes, [leafId]: { ...pane, cwd } } };
    }),

  paneProject: (leafId, project) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane || (pane.project ?? null) === project) return s;
      return { panes: { ...s.panes, [leafId]: { ...pane, project } } };
    }),

  setHomeDir: (home) => set((s) => (s.homeDir === home ? s : { homeDir: home })),

  paneExit: (leafId, exit) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane) return s;
      return { panes: { ...s.panes, [leafId]: { ...pane, exit } } };
    }),

  paneInterventionBadge: (leafId, on) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane || (pane.interventionBadge ?? false) === on) return s;
      return { panes: { ...s.panes, [leafId]: { ...pane, interventionBadge: on } } };
    }),

  paneRelief: (leafId, relief, isProtected) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane) return s;
      const current = pane.relief ?? PANE_RELIEF_NONE;
      if (sameRelief(current, relief) && (pane.protected ?? false) === isProtected) return s;
      return { panes: { ...s.panes, [leafId]: { ...pane, relief, protected: isProtected } } };
    }),

  paneResume: (leafId, resume) =>
    set((s) => {
      const pane = s.panes[leafId];
      if (!pane || (pane.resume ?? null) === resume) return s;
      return { panes: { ...s.panes, [leafId]: { ...pane, resume } } };
    }),

  removePaneMeta: (leafId) =>
    set((s) => {
      if (!s.panes[leafId]) return s;
      const panes = { ...s.panes };
      delete panes[leafId];
      return { panes };
    }),

  setQueue: (queue) => set({ queue }),

  upsertWorkload: (workload) =>
    set((s) => {
      const idx = s.workloads.findIndex((w) => w.workload_id === workload.workload_id);
      const workloads = [...s.workloads];
      if (idx >= 0) workloads[idx] = workload;
      else workloads.push(workload);
      const retained = pruneFinishedWorkloads(workloads, s.panes);
      // 감지된 에이전트는 에이전트가 끝나 요약에서 사라지기 전에 기억해 둔다.
      const workloadMemory = forgetDroppedWorkloads(
        rememberWorkloadAgents(s.workloadMemory, [workload]),
        workloads,
        retained,
      );
      return { workloads: retained, workloadMemory, ...reindexWorkloads(retained) };
    }),

  setHostSample: (sample) => set({ host: sample }),

  applySnapshot: (snapshot) => {
    const s = get();
    // revision은 전역 증가 — 더 오래된 snapshot은 폐기(01 §4).
    if (snapshot.revision < s.revision) return;
    set({
      workloads: snapshot.workloads,
      ...reindexWorkloads(snapshot.workloads),
      // snapshot은 데몬이 기억하는 작업 전부다 — 거기도 없고 pane도 가리키지 않는 작업은 더 부를 곳이 없다.
      workloadMemory: pruneWorkloadMemory(
        rememberWorkloadAgents(s.workloadMemory, snapshot.workloads),
        snapshot.workloads,
        s.panes,
      ),
      queue: snapshot.queue,
      host: snapshot.host,
      revision: snapshot.revision,
      daemonPlatform: snapshot.capabilities.platform,
      missionProtocol: missionProtocolVersion(snapshot.capabilities) ?? null,
      claudeProviderRouting: snapshot.capabilities.claude_provider_routing === true,
      // 구 데몬은 두 필드를 보내지 않는다 — 그때는 "양보 불가"와 기본 정책이다.
      schedulingYield: snapshot.capabilities.scheduling_yield ?? null,
      reliefPolicy: snapshot.relief_policy ?? s.reliefPolicy,
    });
  },

  setReliefPolicy: (policy) =>
    set((s) => (s.reliefPolicy.auto_yield === policy.auto_yield ? s : { reliefPolicy: policy })),

  openModal: (modal) =>
    modal.kind === "mission-create"
      ? set({ missionCreate: modal, modal: null, page: "terminal", settingsGroup: null, renamingTabId: null })
      : modal.kind === "managed-run" ? set({ page: "settings", modal: null, settingsGroup: "run" }) : set({ modal }),
  closeModal: () => set({ modal: null }),
  // 화면이 바뀌면 숨는 쪽의 탭 이름 편집 칸은 입력 초점을 받을 수 없다 — 편집 상태를 남기지 않는다
  // (남으면 window 단축키가 전부 막힌다).
  setPage: (page) => set({ page, modal: null, settingsGroup: null, renamingTabId: null }),
  openSettings: (group) => set({ page: "settings", modal: null, settingsGroup: group, renamingTabId: null }),
  setToast: (message) => set({ toast: message }),
  toggleQueueDrawer: (open) => set((s) => ({ queueDrawerOpen: open ?? !s.queueDrawerOpen })),
  setGraphDrawer: (open) => set({ graphDrawerOpen: open }),
  toggleBroadcastInput: (on) => set((s) => ({ broadcastInput: on ?? !s.broadcastInput })),
}));

/** 잠깐 보이는 안내 토스트의 기본 표시 시간(컨트롤러 토스트와 같다). */
export const FLASH_TOAST_MS = 4000;
let flashToastTimer: ReturnType<typeof setTimeout> | null = null;

/** 스스로 사라지는 토스트의 마지막 주인(flashToast·세션 제어기 toast가 같이 쓴다). */
let timedToastOwner: object | null = null;

/** 스스로 사라질 토스트를 띄우고, 나중에 지울 자격(토큰)을 받는다. 마지막에 띄운 쪽만 주인이다. */
export function showOwnedToast(message: string): object {
  const token = {};
  timedToastOwner = token;
  useWorkbenchStore.getState().setToast(message);
  return token;
}

/**
 * 토큰이 아직 주인이고 지금 토스트가 그때 띄운 문구일 때만 지운다 — 나중에 뜬 토스트(다른 타이머의
 * 것이든 직접 띄운 더 중요한 안내든)를 먼저 띄운 쪽의 타이머가 지우지 않게.
 */
export function clearOwnedToast(token: object, message: string): void {
  if (timedToastOwner !== token) return;
  timedToastOwner = null;
  if (useWorkbenchStore.getState().toast === message) useWorkbenchStore.getState().setToast(null);
}

/**
 * 잠깐 보이는 안내 토스트(탭 닫기 안내·결정 필요 낭독 등). 시간이 지나도 그사이 다른 토스트가
 * 떴으면 지우지 않는다 — 더 중요한 안내(탭 상한 등)나 세션 제어기 토스트를 덮거나 지우지 않게.
 */
export function flashToast(message: string, durationMs: number = FLASH_TOAST_MS): void {
  const token = showOwnedToast(message);
  if (flashToastTimer !== null) clearTimeout(flashToastTimer);
  flashToastTimer = setTimeout(() => {
    flashToastTimer = null;
    clearOwnedToast(token, message);
  }, durationMs);
}

useWorkbenchStore.subscribe(scheduleWorkspaceSave);
useWorkbenchStore.subscribe((state) => scheduleWorkloadMemorySave(state.workloadMemory));
// 간격 안에 모아 둔 마지막 제목을 창이 닫힐 때 잃지 않게 한다.
if (typeof window !== "undefined") {
  window.addEventListener("pagehide", flushWorkloadMemorySave);
  window.addEventListener("pagehide", flushWorkspaceSave);
}

/** mission 계열 탭의 id — 열림 시점에 store 스스로 만든다(호출자 uuid 불필요). */
function newTabId(): string {
  return typeof crypto !== "undefined" && typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `tab-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

/**
 * AI 작업 계열 탭을 열거나 그 탭으로 옮길 때 탭 화면(terminal page)을 보이게 한다 — 설정·배치 편집
 * 화면에서 목록·알림·후속 작업·되돌리기로 연 탭이 가려진 채 남지 않게. 대화상자는 호출자가 닫는다
 * (setPage와 달리 modal은 건드리지 않는다).
 */
function showTabsPage(s: Pick<WorkbenchState, "page">): Partial<WorkbenchState> {
  return s.page === "terminal" ? {} : { page: "terminal", settingsGroup: null, renamingTabId: null };
}

function removeFromSet(set: ReadonlySet<string>, value: string): ReadonlySet<string> {
  if (!set.has(value)) return set;
  const next = new Set(set);
  next.delete(value);
  return next;
}

/**
 * 활성 탭 안의 leaf만 초점을 가질 수 있다(workspaceStorage.decodeWorkspace와
 * 같은 규칙): 지금 초점이 그 탭에 있으면 그대로, 아니면 탭의 첫 leaf.
 * mission/agent-view 탭은 leaf가 없으므로 초점도 없다(05 §2).
 */
type TerminalTabState = Extract<TabState, { kind: "terminal" }>;

/** leaf가 들어 있는 terminal 탭(없으면 null). */
function terminalTabOf(tabs: TabState[], leafId: string): TerminalTabState | null {
  for (const tab of tabs) {
    if (tab.kind === "terminal" && findLeaf(tab.root, leafId) !== null) return tab;
  }
  return null;
}

/** terminal 탭들의 root를 바꾼다. root가 null이 된(비어 버린) 탭은 닫는다 — 옮기기의 목적은 정리다. */
function withRoots(tabs: TabState[], roots: ReadonlyMap<string, SplitNode | null>): TabState[] {
  return tabs
    .map((tab) => (tab.kind === "terminal" && roots.has(tab.id) ? { ...tab, root: roots.get(tab.id) ?? null } : tab))
    .filter((tab) => !(tab.kind === "terminal" && roots.has(tab.id) && tab.root === null));
}

function focusWithinTab(tabs: TabState[], activeTabId: string | null, focusedLeafId: string | null): string | null {
  const tab = tabs.find((t) => t.id === activeTabId);
  if (!tab || tab.kind !== "terminal" || !tab.root) return null;
  if (focusedLeafId !== null && findLeaf(tab.root, focusedLeafId)) return focusedLeafId;
  return nearestLeaf(tab.root, "start");
}

/**
 * 끝난 작업 요약을 이만큼만 남긴다. 데몬 이벤트는 작업을 추가·갱신만 하고
 * 제거 신호가 없어, 하루 종일 터미널을 열고 닫으면 목록이 끝없이 자랐다
 * (대기 뷰의 "최근 종료"는 마지막 10개만 보여 준다). pane이 아직 가리키는
 * 작업(종료 오버레이의 사유 표시)은 개수와 무관하게 남긴다.
 */
export const FINISHED_WORKLOADS_RETAINED = 50;

/**
 * `workloads` 배열의 파생 인덱스(`workloadById`·`workloadBySession`).
 * 세션 중복(이어받기 등)이 있으면 배열 순서에서 **첫** 항목을 고른다 —
 * 기존 `.find` 호출점과 같은 의미를 유지한다.
 */
function reindexWorkloads(
  workloads: WorkloadSummary[],
): Pick<WorkbenchState, "workloadById" | "workloadBySession"> {
  const workloadById = new Map<string, WorkloadSummary>();
  const workloadBySession = new Map<string, WorkloadSummary>();
  for (const workload of workloads) {
    if (!workloadById.has(workload.workload_id)) workloadById.set(workload.workload_id, workload);
    // session_id는 옵셔널(null | undefined) — 값이 있는 것만 색인한다.
    if (workload.session_id != null && !workloadBySession.has(workload.session_id)) {
      workloadBySession.set(workload.session_id, workload);
    }
  }
  return { workloadById, workloadBySession };
}

export function pruneFinishedWorkloads(
  workloads: WorkloadSummary[],
  panes: Record<string, PaneMeta>,
): WorkloadSummary[] {
  const referenced = new Set<string>();
  for (const pane of Object.values(panes)) {
    if (pane.workloadId) referenced.add(pane.workloadId);
  }
  let finished = 0;
  for (const workload of workloads) {
    if (isFinishedWorkload(workload.state) && !referenced.has(workload.workload_id)) finished += 1;
  }
  let drop = finished - FINISHED_WORKLOADS_RETAINED;
  if (drop <= 0) return workloads;
  // 배열 순서 = 삽입 순서 → 앞쪽이 오래된 것이다.
  return workloads.filter((workload) => {
    if (drop > 0 && isFinishedWorkload(workload.state) && !referenced.has(workload.workload_id)) {
      drop -= 1;
      return false;
    }
    return true;
  });
}

function patchLeafIn(
  node: SplitNode,
  leafId: string,
  patch: { session_id?: string | null; view_id?: string },
): SplitNode {
  if (node.kind === "leaf") return node.id === leafId ? { ...node, ...patch } : node;
  return {
    ...node,
    first: patchLeafIn(node.first, leafId, patch),
    second: patchLeafIn(node.second, leafId, patch),
  };
}

function withRatioMut(node: SplitNode, splitId: string, ratio: number): SplitNode {
  if (node.kind === "leaf") return node;
  return {
    ...node,
    ratio: node.id === splitId ? Math.min(1, Math.max(0, ratio)) : node.ratio,
    first: withRatioMut(node.first, splitId, ratio),
    second: withRatioMut(node.second, splitId, ratio),
  };
}

// ---- selectors (split so subscriptions stay narrow — U12)

export const selectActiveTab = (s: WorkbenchState): TabState | null =>
  s.tabs.find((t) => t.id === s.activeTabId) ?? null;

export const selectActiveRoot = (s: WorkbenchState): SplitNode | null => {
  const tab = selectActiveTab(s);
  return tab?.kind === "terminal" ? tab.root : null;
};

export const selectHost = (s: WorkbenchState): HostSample | null => s.host;
