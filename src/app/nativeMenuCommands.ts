/**
 * macOS 메뉴 막대(04-ui §3-2)에서 고른 항목을 controller·store로 보낸다.
 *
 * 팔레트·탭 메뉴·pane 메뉴·상단 바와 같은 동작을 부른다. pane 대상 명령은 보고 있는 탭의 초점 pane을
 * 겨냥한다 — 다른 탭에 남은 초점으로 숨은 창을 건드리지 않는다. 설정·배치 편집 화면에서 터미널
 * 화면에서만 결과가 보이는 명령(새 터미널·분할·터미널 클립보드·찾기·화면 지우기·글꼴·다시 시작·탭 이름
 * 바꾸기)을 고르면 팔레트처럼 먼저 터미널 화면으로 돌아간다. 대화상자가 떠 있으면 정보·종료·언어 말고는
 * 실행하지 않는다 — 그동안 메뉴는 비활성이지만, 상태가 네이티브 메뉴에 닿기 전의 틈에 고른 항목도 막는다.
 */

import { useI18nStore } from "../i18n";
import { paneResumeCommand } from "../features/agentSessions/types";
import { useNotificationStore } from "../features/notifications/notificationStore";
import type { SessionController } from "../features/terminal/sessionController";
import { focusedRepositoryHint, openMissionCreate } from "../features/missions/entry";
import { openMissionList } from "../features/missions/entryPoints";
import { findLeaf } from "../features/terminal/splitTree";
import { useWorkbenchStore } from "../store/workbenchStore";
import { useShellProfileStore } from "../stores/shellProfileStore";
import { runOnTerminalPage } from "./layoutPage";
import type { NativeMenuCommand, NativeMenuTarget } from "./nativeMenuModel";
import { requestCloseTab } from "./tabCommands";

export type NativeMenuController = Pick<
  SessionController,
  | "dispatchShortcut"
  | "newTerminal"
  | "newTab"
  | "requestClosePanes"
  | "requestCloseAllTabs"
  | "copySelection"
  | "pasteFromClipboard"
  | "selectAllInPane"
  | "clearPaneScrollback"
  | "focusPaneTerminal"
  | "copyText"
  | "toggleBroadcast"
  | "detachPaneToNewTab"
  | "retryPane"
  | "moveTab"
  | "regroupByProject"
  | "resumeAllSuspended"
  | "copyMemoryDiagnostics"
  | "requestQuit"
  | "paneFontSize"
  | "paneHasSelection"
>;

/** 셸 목록 항목이 여는 셸 — controller.newTerminal에 그대로 넘긴다. */
export interface NativeMenuShellLaunch {
  program: string;
  argv: string[];
  label: string;
}

export interface NativeMenuCommandDeps {
  controller: NativeMenuController;
  /** 정보 대화상자(런타임 버전을 아는 쪽이 연다). */
  openAbout(): void;
  /** 셸 목록의 프로필 id → 실행할 셸. 목록이 바뀌어 사라졌으면 null. */
  resolveShell(profileId: string): NativeMenuShellLaunch | null;
}

/** 터미널 화면에서만 결과가 보이는 명령 — 다른 화면에서 고르면 먼저 돌아간다. */
const TERMINAL_PAGE_COMMANDS: ReadonlySet<NativeMenuCommand> = new Set<NativeMenuCommand>([
  "new-terminal",
  "split-row",
  "split-column",
  "copy-selection",
  "paste-terminal",
  "select-all-terminal",
  "find",
  "clear-terminal",
  "zoom-in",
  "zoom-out",
  "zoom-reset",
  "restart-pane",
  "rename-tab",
]);

export function runNativeMenuCommand(target: NativeMenuTarget, deps: NativeMenuCommandDeps): void {
  // 언어는 대화상자와 무관한 표시 설정이다.
  if (target.kind === "language") {
    useI18nStore.getState().setLanguage(target.language);
    return;
  }
  const modalOpen = useWorkbenchStore.getState().modal !== null;
  switch (target.kind) {
    case "shell": {
      if (modalOpen) return;
      const shell = deps.resolveShell(target.profileId);
      if (shell) {
        runOnTerminalPage(() => {
          deps.controller.newTerminal(shell);
        })();
      }
      return;
    }
    case "default-shell":
      // 셸 선택기의 "기본으로 설정"과 같다.
      if (!modalOpen) useShellProfileStore.getState().setDefault(target.profileId);
      return;
    case "command": {
      const { command } = target;
      if (modalOpen && command !== "about" && command !== "quit") return;
      const run = () => runCommand(command, deps);
      if (TERMINAL_PAGE_COMMANDS.has(command)) runOnTerminalPage(run)();
      else run();
      return;
    }
  }
}

/** 보고 있는 terminal 탭 안의 초점 pane(없으면 null). */
function focusedPaneInActiveTab(): string | null {
  const state = useWorkbenchStore.getState();
  const tab = state.tabs.find((candidate) => candidate.id === state.activeTabId);
  const leafId = state.focusedLeafId;
  if (leafId === null || tab?.kind !== "terminal" || findLeaf(tab.root, leafId) === null) return null;
  return state.panes[leafId] ? leafId : null;
}

function runCommand(command: NativeMenuCommand, deps: NativeMenuCommandDeps): void {
  const { controller } = deps;
  const store = useWorkbenchStore.getState();
  const activeTab = store.tabs.find((candidate) => candidate.id === store.activeTabId) ?? null;
  const leafId = focusedPaneInActiveTab();
  switch (command) {
    case "about":
      deps.openAbout();
      return;
    case "quit":
      controller.requestQuit();
      return;
    case "settings":
      store.openSettings("general");
      return;
    case "shortcuts":
      store.openSettings("shortcuts");
      return;
    case "new-terminal":
      controller.newTerminal();
      return;
    case "new-tab":
      controller.newTab();
      return;
    case "new-mission":
      // 보고 있는 터미널의 저장소를 채워 연다(mission 탭을 보고 있으면 빈 입력).
      openMissionCreate(focusedRepositoryHint(store));
      return;
    case "mission-list":
      // 닫았던 작업 탭을 다시 여는 자리(작업은 탭과 무관하게 계속 실행된다).
      openMissionList();
      return;
    case "managed-run":
      store.openModal({ kind: "managed-run" });
      return;
    case "agent-sessions":
      store.openModal({ kind: "agent-sessions" });
      return;
    case "resume-all":
      // 결과는 toast·대기열 패널로 보이므로 터미널 화면으로 돌아갈 필요가 없다.
      void controller.resumeAllSuspended();
      return;
    case "close-pane":
      if (leafId) controller.requestClosePanes([leafId]);
      return;
    case "close-tab":
      if (activeTab) requestCloseTab(controller, activeTab.id);
      return;
    case "close-all-tabs":
      controller.requestCloseAllTabs();
      return;
    // 편집 메뉴의 시스템 항목은 입력 초점(DOM)을 따르므로, 초점 pane을 겨냥하는 경로를 따로 둔다
    // (오른쪽 클릭 메뉴와 같은 controller 경로). 끝나면 입력 초점을 그 터미널로 돌려준다.
    case "copy-selection":
      if (leafId) {
        void controller.copySelection();
        controller.focusPaneTerminal(leafId);
      }
      return;
    case "paste-terminal":
      if (leafId) {
        void controller.pasteFromClipboard();
        controller.focusPaneTerminal(leafId);
      }
      return;
    case "select-all-terminal":
      if (leafId) {
        controller.selectAllInPane(leafId);
        controller.focusPaneTerminal(leafId);
      }
      return;
    case "find":
      controller.dispatchShortcut("search");
      return;
    case "clear-terminal":
      if (leafId) controller.clearPaneScrollback(leafId);
      return;
    case "copy-cwd": {
      const cwd = leafId ? store.panes[leafId]?.cwd : null;
      if (cwd) void controller.copyText(cwd);
      return;
    }
    case "copy-resume": {
      const pane = leafId ? store.panes[leafId] : undefined;
      const resume = pane ? paneResumeCommand(pane) : null;
      if (resume) void controller.copyText(resume);
      return;
    }
    case "palette":
      controller.dispatchShortcut("palette");
      return;
    case "layout-editor":
      controller.dispatchShortcut("layout-editor");
      return;
    case "queue":
      // 다른 화면에서 고르면 돌아가서 연다 — 숨은 채 열려 있던 패널을 닫아 버리지 않게.
      if (store.page !== "terminal") {
        store.setPage("terminal");
        store.toggleQueueDrawer(true);
      } else {
        store.toggleQueueDrawer();
      }
      return;
    case "graphs":
      if (store.page !== "terminal") {
        store.setPage("terminal");
        store.setGraphDrawer(true);
      } else {
        store.setGraphDrawer(!store.graphDrawerOpen);
      }
      return;
    case "notifications": {
      // 알림 패널은 터미널 화면 상단 바의 벨에 붙어 있다 — 다른 화면에서 고르면 돌아가서 연다.
      const notifications = useNotificationStore.getState();
      if (store.page !== "terminal") {
        store.setPage("terminal");
        notifications.setPanelOpen(true);
      } else {
        notifications.setPanelOpen(!notifications.panelOpen);
      }
      return;
    }
    case "clear-notifications":
      useNotificationStore.getState().clear();
      return;
    case "zoom-in":
    case "zoom-out":
    case "zoom-reset":
      if (leafId) controller.dispatchShortcut(command);
      return;
    case "split-row":
    case "split-column":
      controller.dispatchShortcut(command);
      return;
    case "broadcast":
      controller.toggleBroadcast();
      return;
    case "detach-pane":
      if (leafId && controller.detachPaneToNewTab(leafId)) {
        // 새 탭에서 다시 그려진 뒤에 초점을 준다(paneMenuActions와 같은 까닭 — 시험 환경엔 rAF가 없다).
        if (typeof requestAnimationFrame === "function") {
          requestAnimationFrame(() => controller.focusPaneTerminal(leafId));
        } else {
          controller.focusPaneTerminal(leafId);
        }
      }
      return;
    case "move-pane":
      if (leafId) store.openModal({ kind: "move-pane", leafId });
      return;
    case "restart-pane":
      if (leafId) controller.retryPane(leafId);
      return;
    case "rename-tab":
      if (activeTab?.kind === "terminal") store.startTabRename(activeTab.id);
      return;
    case "next-tab":
    case "prev-tab":
      // 단축키·트랙패드 스와이프와 같은 경로(설정의 "끝에서 순환"을 따른다).
      controller.dispatchShortcut(command === "next-tab" ? "next-tab" : "prev-tab");
      return;
    case "move-tab-left":
      if (activeTab) controller.moveTab(activeTab.id, -1);
      return;
    case "move-tab-right":
      if (activeTab) controller.moveTab(activeTab.id, 1);
      return;
    case "merge-tab":
      if (activeTab?.kind === "terminal") store.openModal({ kind: "merge-tab", tabId: activeTab.id });
      return;
    case "regroup":
      controller.regroupByProject();
      return;
    case "memory-diagnostics":
      controller.copyMemoryDiagnostics();
      return;
    default: {
      const unreachable: never = command;
      return unreachable;
    }
  }
}
