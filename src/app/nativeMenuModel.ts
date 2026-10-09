/**
 * macOS 메뉴 막대 모델(04-ui §3-2).
 *
 * 무엇을 어떤 순서·상태로 보일지만 정하는 순수 함수다 — Tauri도 React도 모르고, 네이티브 항목을
 * 만들고 고치는 일은 nativeMenu.ts가, 고른 항목의 실행은 nativeMenuCommands.ts가 맡는다. 팔레트·
 * 컨텍스트 메뉴·상단 바에 흩어진 앱 기능을 메뉴 막대 한곳에 모은다. 지금 할 수 없는 항목은 숨기지 않고
 * 비활성으로 둔다 — 조건은 탭 메뉴·pane 메뉴(§2-5·§3-1)와 같다.
 *
 * 단축키 표시는 §3 표에 사용자 재정의를 반영한다. 앱 단축키를 단 항목은 shortcut에 그 액션을 적어
 * 두고, 페이지가 흘려보낸 같은 조합의 되울림은 실행하지 않는다(menuKeyEcho.ts).
 */

import { LANGUAGES, translate, type Language } from "../i18n";
import { paneResumeCommand } from "../features/agentSessions/types";
import { missionEntryState, PRODUCTION_BUILD } from "../features/missions/capability";
import type { PanePhase } from "../features/monitor/statusStrings";
import { canAddPane, findLeaf, leafCount } from "../features/terminal/splitTree";
import { isFinishedWorkload } from "../store/workloadState";
import {
  effectiveBinding,
  type KeyBinding,
  type Platform,
  type ShortcutAction,
  type ShortcutOverrides,
} from "../features/terminal/shortcuts";
import { MAX_FONT_SIZE, MIN_FONT_SIZE } from "../features/terminal/zoom";
import type { WorkbenchState } from "../store/workbenchStore";

/** 메뉴 막대가 부르는 앱 명령 — 하나도 빠짐없이 메뉴 어딘가에 한 번씩 놓인다(시험이 지킨다). */
export const NATIVE_MENU_COMMANDS = [
  "about",
  "settings",
  "quit",
  "new-terminal",
  "new-tab",
  "new-mission",
  "mission-list",
  "managed-run",
  "agent-sessions",
  "resume-all",
  "close-pane",
  "close-tab",
  "close-all-tabs",
  "copy-selection",
  "paste-terminal",
  "select-all-terminal",
  "find",
  "clear-terminal",
  "copy-cwd",
  "copy-resume",
  "palette",
  "layout-editor",
  "queue",
  "graphs",
  "notifications",
  "clear-notifications",
  "zoom-in",
  "zoom-out",
  "zoom-reset",
  "split-row",
  "split-column",
  "broadcast",
  "detach-pane",
  "move-pane",
  "restart-pane",
  "next-tab",
  "prev-tab",
  "rename-tab",
  "move-tab-left",
  "move-tab-right",
  "merge-tab",
  "regroup",
  "shortcuts",
  "memory-diagnostics",
] as const;

export type NativeMenuCommand = (typeof NATIVE_MENU_COMMANDS)[number];

/** 항목을 골랐을 때 할 일. */
export type NativeMenuTarget =
  | { kind: "command"; command: NativeMenuCommand }
  | { kind: "language"; language: Language }
  | { kind: "shell"; profileId: string }
  | { kind: "default-shell"; profileId: string };

export interface NativeMenuItemSpec {
  kind: "item";
  /** 네이티브 항목 id — 메뉴 막대 전체에서 유일하고, 트레이 항목 id와 겹치지 않게 "iyagi-"로 시작한다. */
  id: string;
  target: NativeMenuTarget;
  label: string;
  /** 페이지(중앙 key handler)가 가드와 함께 판정하는 앱 단축키. 없으면 null. */
  shortcut: ShortcutAction | null;
  /** 메뉴에 등록·표시할 조합(Tauri accelerator 문자열). 없으면 null. */
  accelerator: string | null;
  enabled: boolean;
  /** boolean이면 체크 항목. */
  checked?: boolean;
}

/** 시스템이 동작을 가진 항목 — 편집 메뉴는 입력 칸의 편집 단축키와 네이티브 붙여넣기 경로가 이 선택자에 기댄다. */
export type NativeMenuPredefined =
  | "Services"
  | "Hide"
  | "HideOthers"
  | "ShowAll"
  | "Undo"
  | "Redo"
  | "Cut"
  | "Copy"
  | "Paste"
  | "SelectAll"
  | "Fullscreen"
  | "Minimize"
  | "Maximize"
  | "BringAllToFront";

export interface NativeMenuPredefinedSpec {
  kind: "predefined";
  /** 모델 안에서만 쓰는 열쇠(네이티브 id는 시스템이 정한다). */
  id: string;
  item: NativeMenuPredefined;
  label: string;
}

export interface NativeMenuSeparatorSpec {
  kind: "separator";
  id: string;
}

export interface NativeMenuSubmenuSpec {
  kind: "submenu";
  id: string;
  label: string;
  enabled: boolean;
  items: NativeMenuEntrySpec[];
}

export type NativeMenuEntrySpec =
  | NativeMenuItemSpec
  | NativeMenuPredefinedSpec
  | NativeMenuSeparatorSpec
  | NativeMenuSubmenuSpec;

export interface NativeMenuShell {
  id: string;
  /** 화면 언어로 번역한 표시 이름. */
  label: string;
  /** 저장된 기본 셸인가 — 셸 선택기의 "기본" 표시와 같다(아직 고르지 않았으면 모두 false). */
  isDefault: boolean;
}

/** 스토어 밖에서 모으는 값 — 셸 목록·알림·설정, 그리고 초점 pane의 터미널 상태를 읽는 방법. */
export interface NativeMenuContext {
  platform: Platform;
  language: Language;
  overrides: ShortcutOverrides;
  shells: NativeMenuShell[];
  /** 사용자 기본 글꼴 크기(되돌리기 목표). */
  baseFontSize: number;
  notifications: { count: number; open: boolean };
  /** 스토어에 없는 터미널 상태: 지금 글꼴 크기. */
  paneFontSize(leafId: string): number;
  /** 스토어에 없는 터미널 상태: 선택 영역이 있는가. */
  paneHasSelection(leafId: string): boolean;
  /** 프로덕션 빌드인가(생략하면 이 빌드) — 프로토콜 없는 데몬에서 AI 작업 항목을 숨길지 정한다. */
  productionBuild?: boolean;
}

/** 메뉴를 그리는 데 필요한 값만 모은 것(함수 없음) — 이 값이 같으면 메뉴도 같다. */
export interface NativeMenuSnapshot {
  platform: Platform;
  language: Language;
  overrides: ShortcutOverrides;
  shells: NativeMenuShell[];
  baseFontSize: number;
  notifications: { count: number; open: boolean };
  modalOpen: boolean;
  page: WorkbenchState["page"];
  tabCount: number;
  terminalTabCount: number;
  activeTab: {
    kind: WorkbenchState["tabs"][number]["kind"];
    index: number;
    /** terminal 탭에 pane 트리가 있는가(빈 탭·mission 계열 탭은 false). */
    hasPanes: boolean;
    paneCount: number;
    canAddPane: boolean;
  } | null;
  /** 보고 있는 탭의 초점 pane — 다른 탭에 남은 초점은 치지 않는다. */
  focusedPane: {
    phase: PanePhase;
    cwd: string | null;
    resumeCommand: string | null;
    /**
     * 줌 항목이 쓰는 것은 경계 판정 셋뿐이다(상한·하한·기본 크기). 글자 크기
     * 값을 그대로 담으면 스냅샷 키(JSON)가 줌 한 단계마다 달라져 ~100노드
     * 메뉴 트리가 매번 다시 만들어진다 — 실제로 바뀌는 것만 담아 대부분의
     * 줌에서 nativeMenu의 조기 return이 살아 있게 한다.
     */
    canZoomIn: boolean;
    canZoomOut: boolean;
    canZoomReset: boolean;
    hasSelection: boolean;
  } | null;
  missionsReady: boolean;
  /** AI 작업 항목을 숨긴다(프로덕션 빌드 + 데몬이 프로토콜을 선언하지 않음). */
  missionsHidden: boolean;
  broadcast: boolean;
  queueOpen: boolean;
  graphOpen: boolean;
  /** 일시정지 모두 재개 대상 개수 — 개수만 담아 스냅샷 키가 가드 틱마다 달라지지 않게 한다. */
  suspendedCount: number;
}

export type NativeMenuState = Pick<
  WorkbenchState,
  | "tabs"
  | "activeTabId"
  | "focusedLeafId"
  | "panes"
  | "modal"
  | "page"
  | "missionProtocol"
  | "broadcastInput"
  | "queueDrawerOpen"
  | "graphDrawerOpen"
  | "workloads"
>;

export function nativeMenuSnapshot(state: NativeMenuState, context: NativeMenuContext): NativeMenuSnapshot {
  const index = state.tabs.findIndex((tab) => tab.id === state.activeTabId);
  const tab = index >= 0 ? state.tabs[index] : null;
  const root = tab?.kind === "terminal" ? tab.root : null;
  const focusedId = state.focusedLeafId;
  const pane =
    focusedId !== null && root !== null && findLeaf(root, focusedId) !== null ? (state.panes[focusedId] ?? null) : null;
  return {
    platform: context.platform,
    language: context.language,
    overrides: context.overrides,
    shells: context.shells,
    baseFontSize: context.baseFontSize,
    notifications: context.notifications,
    modalOpen: state.modal !== null,
    page: state.page,
    tabCount: state.tabs.length,
    terminalTabCount: state.tabs.filter((candidate) => candidate.kind === "terminal").length,
    activeTab: tab
      ? { kind: tab.kind, index, hasPanes: root !== null, paneCount: leafCount(root), canAddPane: canAddPane(root) }
      : null,
    focusedPane:
      pane && focusedId !== null
        ? {
            phase: pane.phase,
            cwd: pane.cwd,
            resumeCommand: paneResumeCommand(pane),
            ...zoomFlags(context.paneFontSize(focusedId), context.baseFontSize),
            hasSelection: context.paneHasSelection(focusedId),
          }
        : null,
    ...missionMenuFlags(state.missionProtocol, context.productionBuild ?? PRODUCTION_BUILD),
    broadcast: state.broadcastInput,
    queueOpen: state.queueDrawerOpen,
    graphOpen: state.graphDrawerOpen,
    suspendedCount: state.workloads.filter(
      (workload) => !isFinishedWorkload(workload.state) && workload.guard?.kind === "SUSPENDED",
    ).length,
  };
}

function missionMenuFlags(protocol: number | null, production: boolean): { missionsReady: boolean; missionsHidden: boolean } {
  const entry = missionEntryState(protocol, production);
  return { missionsReady: entry.enabled, missionsHidden: entry.hidden };
}

/** 트레이 메뉴 항목("open"·"quit")과 겹치지 않는 네이티브 id. */
export function nativeMenuItemId(command: NativeMenuCommand): string {
  return `iyagi-${command}`;
}

/** Tauri가 앱 메뉴를 걸 때 NSApp의 창 메뉴·도움말 메뉴로 등록하는 submenu id(창 목록·도움말 검색이 붙는다). */
export const WINDOW_SUBMENU_ID = "__tauri_window_menu__";
export const HELP_SUBMENU_ID = "__tauri_help_menu__";

/** 메뉴만 가진 조합 — 페이지 단축키가 아니므로 되울림 거르기 대상이 아니다. */
/** 줌 메뉴 항목의 enabled 판정(스냅샷이 글자 크기 값을 담지 않는 까닭). */
function zoomFlags(fontSize: number, baseFontSize: number): {
  canZoomIn: boolean;
  canZoomOut: boolean;
  canZoomReset: boolean;
} {
  return {
    canZoomIn: fontSize < MAX_FONT_SIZE,
    canZoomOut: fontSize > MIN_FONT_SIZE,
    canZoomReset: fontSize !== baseFontSize,
  };
}

export const SETTINGS_ACCELERATOR = "CmdOrCtrl+,";
export const QUIT_ACCELERATOR = "CmdOrCtrl+Q";

/** 메뉴 막대가 조합으로 그릴 수 있는 키(물리 code). 나머지는 표시만 뺀다. */
const MENU_KEY_CODES: ReadonlySet<string> = new Set([
  ..."ABCDEFGHIJKLMNOPQRSTUVWXYZ".split("").map((letter) => `Key${letter}`),
  ..."0123456789".split("").map((digit) => `Digit${digit}`),
  ...Array.from({ length: 12 }, (_, index) => `F${index + 1}`),
  "Equal",
  "Minus",
  "Comma",
  "Period",
  "Slash",
  "Semicolon",
  "Quote",
  "Backquote",
  "Backslash",
  "BracketLeft",
  "BracketRight",
]);

/**
 * 메뉴 막대에 등록할 조합(Tauri accelerator). Cmd·Ctrl이 없는 조합(터미널 통과키)과 메뉴가 그릴 수
 * 없는 키는 null — 단축키 자체는 페이지가 계속 처리하고, 메뉴에는 표시만 빠진다.
 */
export function menuAccelerator(binding: KeyBinding | null, platform: Platform): string | null {
  if (!binding || (!binding.ctrl && !binding.meta) || !MENU_KEY_CODES.has(binding.code)) return null;
  const parts: string[] = [];
  if (binding.meta) parts.push(platform === "darwin" ? "Cmd" : "Super");
  if (binding.ctrl) parts.push("Ctrl");
  if (binding.shift) parts.push("Shift");
  parts.push(binding.code);
  return parts.join("+");
}

/** 대화상자가 떠 있어도 고를 수 있는 명령: 정보·종료(종료 확인은 스스로 모달을 살핀다). 언어 항목도 늘 켜 둔다. */
const MODAL_SAFE_COMMANDS: ReadonlySet<NativeMenuCommand> = new Set(["about", "quit"]);

/** 언어 이름은 그 언어로 적는다 — 어느 화면 언어에서도 자기 언어를 알아본다. */
const LANGUAGE_NAMES: Record<Language, string> = { ko: "한국어", en: "English" };

interface CommandOptions {
  enabled?: boolean;
  shortcut?: ShortcutAction;
  accelerator?: string;
  checked?: boolean;
}

export function buildNativeMenu(s: NativeMenuSnapshot): NativeMenuSubmenuSpec[] {
  const text = (key: string) => translate(s.language, `menu.native.${key}`);
  let separators = 0;
  const separator = (): NativeMenuSeparatorSpec => {
    separators += 1;
    return { kind: "separator", id: `separator-${separators}` };
  };
  const predefined = (item: NativeMenuPredefined, key: string): NativeMenuPredefinedSpec => ({
    kind: "predefined",
    id: `predefined-${item}`,
    item,
    label: text(key),
  });
  const command = (id: NativeMenuCommand, key: string, options: CommandOptions = {}): NativeMenuItemSpec => {
    const shortcut = options.shortcut ?? null;
    return {
      kind: "item",
      id: nativeMenuItemId(id),
      target: { kind: "command", command: id },
      label: text(key),
      shortcut,
      accelerator: shortcut
        ? menuAccelerator(effectiveBinding(shortcut, s.platform, s.overrides), s.platform)
        : (options.accelerator ?? null),
      enabled: (options.enabled ?? true) && (!s.modalOpen || MODAL_SAFE_COMMANDS.has(id)),
      ...(options.checked === undefined ? {} : { checked: options.checked }),
    };
  };
  const submenu = (id: string, label: string, items: NativeMenuEntrySpec[], enabled = true): NativeMenuSubmenuSpec => ({
    kind: "submenu",
    id,
    label,
    enabled,
    items,
  });

  const tab = s.activeTab;
  const pane = s.focusedPane;
  const terminalTab = tab !== null && tab.kind === "terminal";
  // 새 창은 보고 있는 terminal 탭에 창이 있으면 초점 pane 옆으로(탭 상한 전까지), 창이 없거나 mission 계열
  // 탭이면 새 터미널로 시작한다(splitFocused·newTerminal).
  const canSplit = tab !== null && tab.kind === "terminal" && tab.hasPanes ? pane !== null && tab.canAddPane : true;
  const clearable = pane !== null && pane.phase !== "starting" && pane.phase !== "replaying";
  const shellsReady = s.shells.length > 0 && !s.modalOpen;

  const shellItems: NativeMenuItemSpec[] = s.shells.map((shell) => ({
    kind: "item",
    id: `iyagi-shell:${shell.id}`,
    target: { kind: "shell", profileId: shell.id },
    label: shell.label,
    shortcut: null,
    accelerator: null,
    enabled: !s.modalOpen,
  }));
  const defaultShellItems: NativeMenuItemSpec[] = s.shells.map((shell) => ({
    kind: "item",
    id: `iyagi-default-shell:${shell.id}`,
    target: { kind: "default-shell", profileId: shell.id },
    label: shell.label,
    shortcut: null,
    accelerator: null,
    enabled: !s.modalOpen,
    checked: shell.isDefault,
  }));
  const languageItems: NativeMenuItemSpec[] = LANGUAGES.map((language) => ({
    kind: "item",
    id: `iyagi-language-${language}`,
    target: { kind: "language", language },
    label: LANGUAGE_NAMES[language],
    shortcut: null,
    accelerator: null,
    enabled: true,
    checked: s.language === language,
  }));

  return [
    // 첫 메뉴의 제목은 macOS가 앱 이름으로 그린다.
    submenu("iyagi-menu-app", translate(s.language, "app.name"), [
      command("about", "about"),
      separator(),
      command("settings", "settings", { accelerator: SETTINGS_ACCELERATOR }),
      separator(),
      predefined("Services", "services"),
      separator(),
      predefined("Hide", "hide"),
      predefined("HideOthers", "hideOthers"),
      predefined("ShowAll", "showAll"),
      separator(),
      command("quit", "quit", { accelerator: QUIT_ACCELERATOR }),
    ]),
    submenu("iyagi-menu-file", text("file"), [
      command("new-terminal", "newTerminal", { enabled: canSplit }),
      submenu("iyagi-menu-shells", text("newTerminalWith"), shellItems, shellsReady && canSplit),
      submenu("iyagi-menu-default-shell", text("defaultShell"), defaultShellItems, shellsReady),
      command("new-tab", "newTab"),
      // AI 작업: 프로덕션 빌드에서 데몬이 프로토콜을 선언하지 않으면 두 항목을 숨긴다(개발 빌드는 비활성).
      ...(s.missionsHidden
        ? []
        : [
            command("new-mission", "newMission", { shortcut: "new-mission", enabled: s.missionsReady }),
            command("mission-list", "missionList", { enabled: s.missionsReady }),
          ]),
      separator(),
      command("managed-run", "managedRun"),
      command("agent-sessions", "agentSessions"),
      // 일시정지 모두 재개(08 §5) — 일시정지 중인 작업이 있을 때만 켜진다.
      command("resume-all", "resumeAll", { shortcut: "resume-all", enabled: s.suspendedCount > 0 }),
      separator(),
      // 기본 메뉴의 "창 닫기"(Cmd+W)는 두지 않는다 — Cmd+W는 pane 닫기이고 창 닫기는 앱 종료 확인과 같다.
      command("close-pane", "closePane", { shortcut: "close-pane", enabled: pane !== null }),
      command("close-tab", "closeTab", { enabled: tab !== null }),
      command("close-all-tabs", "closeAllTabs", { enabled: s.tabCount > 0 }),
    ]),
    submenu("iyagi-menu-edit", text("edit"), [
      predefined("Undo", "undo"),
      predefined("Redo", "redo"),
      separator(),
      predefined("Cut", "cut"),
      predefined("Copy", "copy"),
      predefined("Paste", "paste"),
      predefined("SelectAll", "selectAll"),
      separator(),
      // 위 시스템 항목은 입력 초점(DOM)을, 아래 셋은 초점 pane을 겨냥한다(오른쪽 클릭 메뉴의 클립보드 구획과 같다).
      command("copy-selection", "copySelection", { enabled: pane !== null && pane.hasSelection }),
      command("paste-terminal", "pasteTerminal", { enabled: pane !== null && pane.phase === "live" }),
      command("select-all-terminal", "selectAllTerminal", { enabled: pane !== null }),
      separator(),
      command("find", "find", { shortcut: "search" }),
      command("clear-terminal", "clearTerminal", { enabled: clearable }),
      separator(),
      command("copy-cwd", "copyCwd", { enabled: Boolean(pane?.cwd) }),
      command("copy-resume", "copyResume", { enabled: Boolean(pane?.resumeCommand) }),
    ]),
    submenu("iyagi-menu-view", text("view"), [
      command("palette", "palette", { shortcut: "palette" }),
      command("layout-editor", "layoutEditor", { shortcut: "layout-editor", checked: s.page === "layout" }),
      separator(),
      command("queue", "queue", { shortcut: "queue-toggle", checked: s.queueOpen }),
      command("graphs", "graphs", { checked: s.graphOpen }),
      command("notifications", "notifications", {
        checked: s.notifications.open,
        enabled: s.notifications.count > 0 || s.notifications.open,
      }),
      command("clear-notifications", "clearNotifications", { enabled: s.notifications.count > 0 }),
      separator(),
      command("zoom-in", "zoomIn", { shortcut: "zoom-in", enabled: pane?.canZoomIn === true }),
      command("zoom-out", "zoomOut", { shortcut: "zoom-out", enabled: pane?.canZoomOut === true }),
      command("zoom-reset", "zoomReset", {
        shortcut: "zoom-reset",
        enabled: pane?.canZoomReset === true,
      }),
      separator(),
      submenu("iyagi-menu-language", text("language"), languageItems),
      separator(),
      predefined("Fullscreen", "fullscreen"),
    ]),
    submenu("iyagi-menu-terminal", text("terminal"), [
      command("split-row", "splitRow", { shortcut: "split-row", enabled: canSplit }),
      command("split-column", "splitColumn", { shortcut: "split-column", enabled: canSplit }),
      separator(),
      command("broadcast", "broadcast", { shortcut: "broadcast-toggle", checked: s.broadcast }),
      separator(),
      command("detach-pane", "detachPane", { enabled: pane !== null && tab !== null && tab.paneCount > 1 }),
      command("move-pane", "movePane", { enabled: pane !== null && s.terminalTabCount > 1 }),
      separator(),
      command("restart-pane", "restartPane", {
        enabled: pane !== null && (pane.phase === "exited" || pane.phase === "failed"),
      }),
    ]),
    submenu("iyagi-menu-tab", text("tab"), [
      // 탭 순환 — 트랙패드 두 손가락 가로 스와이프와 같은 동작이다.
      command("next-tab", "nextTab", { shortcut: "next-tab", enabled: s.tabCount > 1 }),
      command("prev-tab", "prevTab", { shortcut: "prev-tab", enabled: s.tabCount > 1 }),
      separator(),
      command("rename-tab", "renameTab", { enabled: terminalTab }),
      command("move-tab-left", "moveTabLeft", { enabled: tab !== null && tab.index > 0 }),
      command("move-tab-right", "moveTabRight", { enabled: tab !== null && tab.index < s.tabCount - 1 }),
      separator(),
      command("merge-tab", "mergeTab", { enabled: terminalTab && s.terminalTabCount > 1 }),
      command("regroup", "regroup", { enabled: s.terminalTabCount > 1 }),
    ]),
    submenu(WINDOW_SUBMENU_ID, text("window"), [
      predefined("Minimize", "minimize"),
      predefined("Maximize", "zoom"),
      separator(),
      predefined("BringAllToFront", "bringAllToFront"),
    ]),
    submenu(HELP_SUBMENU_ID, text("help"), [
      command("shortcuts", "shortcuts"),
      command("memory-diagnostics", "memoryDiagnostics"),
    ]),
  ];
}

/** 메뉴 트리의 항목(item)을 보이는 순서대로(submenu 안까지). */
export function menuItems(entries: readonly NativeMenuEntrySpec[]): NativeMenuItemSpec[] {
  const items: NativeMenuItemSpec[] = [];
  for (const entry of entries) {
    if (entry.kind === "item") items.push(entry);
    else if (entry.kind === "submenu") items.push(...menuItems(entry.items));
  }
  return items;
}
