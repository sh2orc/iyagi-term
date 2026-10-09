/**
 * 메뉴 막대 명령 배분(04-ui §3-2): 팔레트·탭/pane 메뉴·상단 바와 같은 controller·store 동작을 부르고,
 * pane 명령은 보고 있는 탭의 초점 pane만 겨냥하며, 대화상자가 떠 있으면 기다리고, 다른 화면에서는
 * 터미널 화면에서만 보이는 명령을 고르면 먼저 돌아간다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useI18nStore } from "../i18n";
import { useNotificationStore, type NotificationItem } from "../features/notifications/notificationStore";
import { makeLeaf, type SplitNode } from "../features/terminal/splitTree";
import type { ShortcutAction } from "../features/terminal/shortcuts";
import { useWorkbenchStore, type PaneMeta } from "../store/workbenchStore";
import { useShellProfileStore } from "../stores/shellProfileStore";
import { runNativeMenuCommand, type NativeMenuCommandDeps, type NativeMenuShellLaunch } from "./nativeMenuCommands";
import type { NativeMenuCommand, NativeMenuTarget } from "./nativeMenuModel";

function fakeController() {
  return {
    dispatchShortcut: vi.fn((_action: ShortcutAction) => undefined),
    newTerminal: vi.fn((_shell?: { program: string; argv: string[]; label: string }) => true),
    newTab: vi.fn(() => "tab-new"),
    requestClosePanes: vi.fn((_leafIds: string[], _tabId?: string) => undefined),
    requestCloseAllTabs: vi.fn(() => undefined),
    copySelection: vi.fn(async () => true),
    pasteFromClipboard: vi.fn(async () => undefined),
    selectAllInPane: vi.fn((_leafId: string) => undefined),
    clearPaneScrollback: vi.fn((_leafId: string) => undefined),
    focusPaneTerminal: vi.fn((_leafId: string) => undefined),
    copyText: vi.fn(async (_text: string) => true),
    toggleBroadcast: vi.fn((_on?: boolean) => undefined),
    detachPaneToNewTab: vi.fn((_leafId: string, _atIndex?: number) => true),
    retryPane: vi.fn((_leafId: string) => undefined),
    moveTab: vi.fn((_tabId: string, _delta: number) => undefined),
    regroupByProject: vi.fn(() => undefined),
    resumeAllSuspended: vi.fn(async () => undefined),
    copyMemoryDiagnostics: vi.fn(() => true),
    requestQuit: vi.fn(() => undefined),
    paneFontSize: vi.fn((_leafId: string) => 14),
    paneHasSelection: vi.fn((_leafId: string) => false),
  };
}

function setup(resolveShell: (profileId: string) => NativeMenuShellLaunch | null = () => null) {
  const controller = fakeController();
  const openAbout = vi.fn();
  const deps: NativeMenuCommandDeps = { controller, openAbout, resolveShell };
  const run = (target: NativeMenuTarget | NativeMenuCommand) =>
    runNativeMenuCommand(typeof target === "string" ? { kind: "command", command: target } : target, deps);
  return { controller, openAbout, run };
}

function row(ids: readonly string[]): SplitNode {
  const head = ids[0] ?? "leaf";
  const leaf = makeLeaf(head, `view-${head}`);
  if (ids.length <= 1) return leaf;
  return { kind: "split", id: `split-${head}`, axis: "row", ratio: 0.5, first: leaf, second: row(ids.slice(1)) };
}

function pane(leafId: string, patch: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId,
    viewId: `view-${leafId}`,
    sessionId: null,
    title: leafId,
    cwd: null,
    phase: "live",
    usage: null,
    ...patch,
  } as PaneMeta;
}

const notification: NotificationItem = {
  id: "n1",
  kind: "workload-finished",
  title: "build finished",
  detail: null,
  at: "2026-09-14T00:00:00.000Z",
  sessionId: null,
  acknowledged: false,
};

const chrome = {
  modal: null,
  page: "terminal" as const,
  settingsGroup: null,
  renamingTabId: null,
  queueDrawerOpen: false,
  graphDrawerOpen: false,
};

function resetOtherStores(): void {
  useI18nStore.getState().setLanguage(null);
  useNotificationStore.setState({ items: [], panelOpen: false });
  useShellProfileStore.setState({ defaultProfileId: null, custom: [] });
}

beforeEach(() => {
  resetOtherStores();
  useWorkbenchStore.setState({
    ...chrome,
    tabs: [
      { kind: "terminal", id: "t1", title: "one", root: row(["a", "b"]) },
      { kind: "terminal", id: "t2", title: "two", root: row(["c"]) },
    ],
    activeTabId: "t1",
    focusedLeafId: "a",
    panes: {
      a: pane("a", { cwd: "/work/app", resume: { agent: "claude", agentSessionId: "abc" } as PaneMeta["resume"] }),
      b: pane("b"),
      c: pane("c"),
    },
  });
});

afterEach(() => {
  useWorkbenchStore.setState({ ...chrome, tabs: [], activeTabId: null, focusedLeafId: null, panes: {} });
  resetOtherStores();
});

describe("runNativeMenuCommand", () => {
  it("closes the active terminal tab through the pane close confirmation", () => {
    const { controller, run } = setup();
    run("close-tab");
    expect(controller.requestClosePanes).toHaveBeenCalledWith(["a", "b"], "t1");
  });

  it("hides a mission tab without a terminal confirmation", () => {
    const { controller, run } = setup();
    useWorkbenchStore.setState((s) => ({
      tabs: [...s.tabs, { kind: "mission", id: "m1", title: "mission", missionId: "mission-1" }],
      activeTabId: "m1",
    }));
    run("close-tab");
    expect(controller.requestClosePanes).not.toHaveBeenCalled();
    expect(useWorkbenchStore.getState().tabs.some((tab) => tab.id === "m1")).toBe(false);
  });

  it("aims pane commands at the focused pane of the tab being viewed", () => {
    const { controller, run } = setup();
    run("close-pane");
    run("clear-terminal");
    run("restart-pane");
    run("copy-cwd");
    run("copy-resume");
    run("zoom-in");
    run("detach-pane");
    expect(controller.requestClosePanes).toHaveBeenCalledWith(["a"]);
    expect(controller.clearPaneScrollback).toHaveBeenCalledWith("a");
    expect(controller.retryPane).toHaveBeenCalledWith("a");
    expect(controller.copyText).toHaveBeenNthCalledWith(1, "/work/app");
    expect(controller.copyText).toHaveBeenNthCalledWith(2, "claude --resume abc");
    expect(controller.dispatchShortcut).toHaveBeenCalledWith("zoom-in");
    expect(controller.detachPaneToNewTab).toHaveBeenCalledWith("a");
    expect(controller.focusPaneTerminal).toHaveBeenCalledWith("a");
    run("move-pane");
    expect(useWorkbenchStore.getState().modal).toEqual({ kind: "move-pane", leafId: "a" });
  });

  it("aims the terminal clipboard items at the focused pane, from any page, and gives it the input focus", () => {
    const { controller, run } = setup();
    useWorkbenchStore.setState({ page: "layout" });
    run("paste-terminal");
    expect(useWorkbenchStore.getState().page).toBe("terminal");
    run("copy-selection");
    run("select-all-terminal");
    expect(controller.pasteFromClipboard).toHaveBeenCalledOnce();
    expect(controller.copySelection).toHaveBeenCalledOnce();
    expect(controller.selectAllInPane).toHaveBeenCalledWith("a");
    expect(controller.focusPaneTerminal.mock.calls).toEqual([["a"], ["a"], ["a"]]);
  });

  it("leaves hidden panes alone when the focus is stale in another tab", () => {
    const { controller, run } = setup();
    useWorkbenchStore.setState({ activeTabId: "t2", focusedLeafId: "a" });
    for (const command of [
      "close-pane",
      "clear-terminal",
      "zoom-in",
      "restart-pane",
      "detach-pane",
      "copy-cwd",
      "copy-selection",
      "paste-terminal",
      "select-all-terminal",
      "move-pane",
    ] as const) {
      run(command);
    }
    expect(controller.requestClosePanes).not.toHaveBeenCalled();
    expect(controller.clearPaneScrollback).not.toHaveBeenCalled();
    expect(controller.dispatchShortcut).not.toHaveBeenCalled();
    expect(controller.retryPane).not.toHaveBeenCalled();
    expect(controller.detachPaneToNewTab).not.toHaveBeenCalled();
    expect(controller.copyText).not.toHaveBeenCalled();
    expect(controller.copySelection).not.toHaveBeenCalled();
    expect(controller.pasteFromClipboard).not.toHaveBeenCalled();
    expect(controller.selectAllInPane).not.toHaveBeenCalled();
    expect(useWorkbenchStore.getState().modal).toBeNull();
  });

  it("returns to the terminal page first for commands whose result only shows there", () => {
    const { controller, run } = setup();
    useWorkbenchStore.setState({ page: "layout" });
    run("split-row");
    expect(useWorkbenchStore.getState().page).toBe("terminal");
    expect(controller.dispatchShortcut).toHaveBeenCalledWith("split-row");
    // 배치 편집 화면에서도 보이는 구조 변경은 그 화면에서 그대로 한다.
    useWorkbenchStore.setState({ page: "layout" });
    run("close-pane");
    run("move-tab-right");
    expect(useWorkbenchStore.getState().page).toBe("layout");
    expect(controller.requestClosePanes).toHaveBeenCalledWith(["a"]);
    expect(controller.moveTab).toHaveBeenCalledWith("t1", 1);
  });

  it("toggles panels on the terminal page and opens them when chosen from another page", () => {
    const { run } = setup();
    run("queue");
    expect(useWorkbenchStore.getState().queueDrawerOpen).toBe(true);
    run("queue");
    expect(useWorkbenchStore.getState().queueDrawerOpen).toBe(false);
    useWorkbenchStore.setState({ page: "settings", graphDrawerOpen: true });
    run("graphs");
    expect(useWorkbenchStore.getState()).toMatchObject({ page: "terminal", graphDrawerOpen: true });
    run("graphs");
    expect(useWorkbenchStore.getState().graphDrawerOpen).toBe(false);
  });

  it("toggles the notification panel like the bell, opens it from other pages, and clears", () => {
    const { run } = setup();
    useNotificationStore.setState({ items: [notification] });
    run("notifications");
    expect(useNotificationStore.getState().panelOpen).toBe(true);
    // 벨처럼 열면 모두 읽음이 된다.
    expect(useNotificationStore.getState().items[0]?.acknowledged).toBe(true);
    run("notifications");
    expect(useNotificationStore.getState().panelOpen).toBe(false);
    useWorkbenchStore.setState({ page: "settings" });
    run("notifications");
    expect(useWorkbenchStore.getState().page).toBe("terminal");
    expect(useNotificationStore.getState().panelOpen).toBe(true);
    run("clear-notifications");
    expect(useNotificationStore.getState().items).toEqual([]);
  });

  it("opens settings at the requested group", () => {
    const { run } = setup();
    run("shortcuts");
    expect(useWorkbenchStore.getState()).toMatchObject({ page: "settings", settingsGroup: "shortcuts" });
    run("settings");
    expect(useWorkbenchStore.getState().settingsGroup).toBe("general");
  });

  it("waits while a dialog is open, except About, Quit and the language", () => {
    const { controller, openAbout, run } = setup(() => ({ program: "/bin/zsh", argv: [], label: "zsh" }));
    useWorkbenchStore.setState({ modal: { kind: "palette" } });
    run("split-row");
    run("settings");
    run("notifications");
    run({ kind: "shell", profileId: "zsh" });
    run({ kind: "default-shell", profileId: "zsh" });
    expect(controller.dispatchShortcut).not.toHaveBeenCalled();
    expect(controller.newTerminal).not.toHaveBeenCalled();
    expect(useWorkbenchStore.getState().page).toBe("terminal");
    expect(useNotificationStore.getState().panelOpen).toBe(false);
    expect(useShellProfileStore.getState().defaultProfileId).toBeNull();
    run("about");
    run("quit");
    run({ kind: "language", language: "en" });
    expect(openAbout).toHaveBeenCalledOnce();
    expect(controller.requestQuit).toHaveBeenCalledOnce();
    expect(useI18nStore.getState().language).toBe("en");
  });

  it("starts a terminal with the chosen shell, from any page", () => {
    const zsh = { program: "/bin/zsh", argv: ["-l"], label: "zsh" };
    const { controller, run } = setup((profileId) => (profileId === "zsh" ? zsh : null));
    useWorkbenchStore.setState({ page: "layout" });
    run({ kind: "shell", profileId: "zsh" });
    expect(useWorkbenchStore.getState().page).toBe("terminal");
    expect(controller.newTerminal).toHaveBeenCalledWith(zsh);
    run({ kind: "shell", profileId: "removed" });
    expect(controller.newTerminal).toHaveBeenCalledOnce();
  });

  it("makes a shell the default like the shell picker", () => {
    const { run } = setup();
    run({ kind: "default-shell", profileId: "fish" });
    expect(useShellProfileStore.getState().defaultProfileId).toBe("fish");
  });

  it("탭 순환 항목은 단축키와 같은 경로로 간다", () => {
    const { controller, run } = setup();
    run("next-tab");
    expect(controller.dispatchShortcut).toHaveBeenCalledWith("next-tab");
    run("prev-tab");
    expect(controller.dispatchShortcut).toHaveBeenCalledWith("prev-tab");
  });

  it("maps tab commands to the active tab", () => {
    const { controller, run } = setup();
    run("move-tab-left");
    expect(controller.moveTab).toHaveBeenCalledWith("t1", -1);
    run("rename-tab");
    expect(useWorkbenchStore.getState().renamingTabId).toBe("t1");
    run("merge-tab");
    expect(useWorkbenchStore.getState().modal).toEqual({ kind: "merge-tab", tabId: "t1" });
  });

  it("uses the same controller paths as the palette for the rest", () => {
    const { controller, run } = setup();
    run("new-terminal");
    run("new-tab");
    run("find");
    run("palette");
    run("layout-editor");
    run("split-column");
    run("zoom-reset");
    run("broadcast");
    run("regroup");
    run("memory-diagnostics");
    run("close-all-tabs");
    expect(controller.newTerminal).toHaveBeenCalledWith();
    expect(controller.newTab).toHaveBeenCalledOnce();
    expect(controller.dispatchShortcut.mock.calls.map(([action]) => action)).toEqual([
      "search",
      "palette",
      "layout-editor",
      "split-column",
      "zoom-reset",
    ]);
    expect(controller.toggleBroadcast).toHaveBeenCalledOnce();
    expect(controller.regroupByProject).toHaveBeenCalledOnce();
    expect(controller.copyMemoryDiagnostics).toHaveBeenCalledOnce();
    expect(controller.requestCloseAllTabs).toHaveBeenCalledOnce();
  });

  it("opens the same dialogs as the palette", () => {
    const { run } = setup();
    run("agent-sessions");
    expect(useWorkbenchStore.getState().modal).toEqual({ kind: "agent-sessions" });
    useWorkbenchStore.setState({ modal: null });
    run("new-mission");
    // 보고 있는 탭의 초점 pane(a)의 저장소를 채워 연다.
    expect(useWorkbenchStore.getState().missionCreate).toEqual({ kind: "mission-create", repositoryPath: "/work/app" });
    useWorkbenchStore.setState({ modal: null });
    run("mission-list");
    expect(useWorkbenchStore.getState().modal).toEqual({ kind: "mission-list" });
    useWorkbenchStore.setState({ modal: null });
    run("managed-run");
    expect(useWorkbenchStore.getState()).toMatchObject({ page: "settings", settingsGroup: "run", modal: null });
  });

  it("fills the new AI mission dialog from the viewed terminal's repository only", () => {
    const { run } = setup();
    useWorkbenchStore.setState((s) => ({
      panes: { ...s.panes, b: pane("b", { cwd: "/work/app/src", project: "/work/app-root" }) },
      focusedLeafId: "b",
    }));
    run("new-mission");
    expect(useWorkbenchStore.getState().missionCreate).toEqual({ kind: "mission-create", repositoryPath: "/work/app-root" });
    useWorkbenchStore.setState({
      modal: null,
      tabs: [
        ...useWorkbenchStore.getState().tabs,
        { kind: "mission", id: "m1", title: "mission", missionId: "mission-1" },
      ],
      activeTabId: "m1",
    });
    run("new-mission");
    expect(useWorkbenchStore.getState().missionCreate).toEqual({ kind: "mission-create", repositoryPath: null });
  });
});
