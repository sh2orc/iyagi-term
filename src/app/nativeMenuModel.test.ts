/**
 * macOS 메뉴 막대 모델(04-ui §3-2): 앱 명령이 빠짐없이 한 번씩 놓이고, 단축키 표시는 §3 표와 사용자
 * 재정의를 따르며, 할 수 없는 항목은 비활성·체크 표시는 상태를 따른다.
 */

import { describe, expect, it } from "vitest";
import { missionsAvailable } from "../features/missions/capability";
import { makeLeaf, type SplitNode } from "../features/terminal/splitTree";
import { MAX_FONT_SIZE, MIN_FONT_SIZE } from "../features/terminal/zoom";
import type { WorkloadSummary } from "../generated/WorkloadSummary";
import type { PaneMeta, TabState } from "../store/workbenchStore";
import {
  buildNativeMenu,
  HELP_SUBMENU_ID,
  menuAccelerator,
  menuItems,
  NATIVE_MENU_COMMANDS,
  nativeMenuItemId,
  nativeMenuSnapshot,
  WINDOW_SUBMENU_ID,
  type NativeMenuCommand,
  type NativeMenuContext,
  type NativeMenuItemSpec,
  type NativeMenuShell,
  type NativeMenuState,
  type NativeMenuSubmenuSpec,
} from "./nativeMenuModel";

const BASE_FONT = Math.round((MIN_FONT_SIZE + MAX_FONT_SIZE) / 2);

function row(ids: readonly string[]): SplitNode {
  const head = ids[0] ?? "leaf";
  const leaf = makeLeaf(head, `view-${head}`);
  if (ids.length <= 1) return leaf;
  return { kind: "split", id: `split-${head}`, axis: "row", ratio: 0.5, first: leaf, second: row(ids.slice(1)) };
}

function terminalTab(id: string, leaves: readonly string[]): TabState {
  return { kind: "terminal", id, title: id, root: leaves.length > 0 ? row(leaves) : null };
}

function pane(leafId: string, patch: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId,
    viewId: `view-${leafId}`,
    sessionId: null,
    title: leafId,
    cwd: "/work/app",
    phase: "live",
    usage: null,
    ...patch,
  } as PaneMeta;
}

const suspendedGuard = {
  kind: "SUSPENDED",
  since_ms: "1",
  reason: "host_memory_pressure",
  manual: false,
  partial: false,
} as const;

/** 모델이 보는 필드(state·guard)만 채운 최소 요약. */
function workload(id: string, patch: Partial<WorkloadSummary> = {}): WorkloadSummary {
  return {
    workload_id: id,
    mode: "shell",
    state: "RUNNING",
    priority: 1,
    title: id,
    cwd: "/tmp",
    program: "/bin/zsh",
    reservation_bytes: "0",
    cpu_slots: 1,
    enforcement: "observe",
    root_exited: false,
    cancel_requested: false,
    connection: "detached",
    relief: { kind: "NONE" },
    protected: false,
    guard: { kind: "NONE" },
    ...patch,
  } as WorkloadSummary;
}

function state(patch: Partial<NativeMenuState> = {}): NativeMenuState {
  return {
    tabs: [],
    activeTabId: null,
    focusedLeafId: null,
    panes: {},
    modal: null,
    page: "terminal",
    missionProtocol: null,
    broadcastInput: false,
    queueDrawerOpen: false,
    graphDrawerOpen: false,
    workloads: [],
    ...patch,
  };
}

function snapshotOf(patch: Partial<NativeMenuState> = {}, context: Partial<NativeMenuContext> = {}) {
  return nativeMenuSnapshot(state(patch), {
    platform: "darwin",
    language: "ko",
    overrides: {},
    shells: [],
    baseFontSize: BASE_FONT,
    notifications: { count: 0, open: false },
    paneFontSize: () => BASE_FONT,
    paneHasSelection: () => false,
    ...context,
  });
}

function menu(patch: Partial<NativeMenuState> = {}, context: Partial<NativeMenuContext> = {}): NativeMenuSubmenuSpec[] {
  return buildNativeMenu(snapshotOf(patch, context));
}

function item(bar: NativeMenuSubmenuSpec[], command: NativeMenuCommand): NativeMenuItemSpec {
  const found = menuItems(bar).find((entry) => entry.id === nativeMenuItemId(command));
  if (!found) throw new Error(`menu bar has no ${command}`);
  return found;
}

const zsh: NativeMenuShell = { id: "zsh", label: "zsh", isDefault: true };
const fullTab = {
  tabs: [terminalTab("t1", ["a", "b", "c", "d", "e", "f", "g", "h"])],
  activeTabId: "t1",
  focusedLeafId: "a",
  panes: { a: pane("a") },
};

describe("native menu bar model", () => {
  it("puts every app command on the menu bar exactly once, with unique native ids", () => {
    const bar = menu({}, { shells: [zsh] });
    const commands = menuItems(bar).flatMap((entry) => (entry.target.kind === "command" ? [entry.target.command] : []));
    expect([...commands].sort()).toEqual([...NATIVE_MENU_COMMANDS].sort());
    const ids = menuItems(bar).map((entry) => entry.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(ids.every((id) => id.startsWith("iyagi-"))).toBe(true);
  });

  it("orders the menus like a macOS app, in the UI language", () => {
    expect(menu().map((submenu) => submenu.label)).toEqual(["IYAGI Term", "파일", "편집", "보기", "터미널", "탭", "창", "도움말"]);
    const english = menu({}, { language: "en" });
    expect(english.map((submenu) => submenu.label)).toEqual(["IYAGI Term", "File", "Edit", "View", "Terminal", "Tab", "Window", "Help"]);
    expect(item(english, "split-row").label).toBe("Split Right");
    expect(english[english.length - 2]?.id).toBe(WINDOW_SUBMENU_ID);
    expect(english[english.length - 1]?.id).toBe(HELP_SUBMENU_ID);
  });

  it("keeps the system editing items the webview relies on and gives Cmd+W only to Close Pane", () => {
    const bar = menu();
    const edit = bar[2];
    expect(edit?.items.flatMap((entry) => (entry.kind === "predefined" ? [entry.item] : []))).toEqual([
      "Undo",
      "Redo",
      "Cut",
      "Copy",
      "Paste",
      "SelectAll",
    ]);
    const cmdW = menuItems(bar).filter((entry) => entry.accelerator === "Cmd+KeyW").map((entry) => entry.id);
    expect(cmdW).toEqual([nativeMenuItemId("close-pane")]);
  });

  it("shows the shortcut table on app shortcuts and marks them as page-owned", () => {
    const bar = menu();
    expect(item(bar, "split-row")).toMatchObject({ accelerator: "Cmd+KeyD", shortcut: "split-row" });
    expect(item(bar, "split-column").accelerator).toBe("Cmd+Shift+KeyD");
    expect(item(bar, "close-pane").accelerator).toBe("Cmd+KeyW");
    expect(item(bar, "find")).toMatchObject({ accelerator: "Cmd+KeyF", shortcut: "search" });
    expect(item(bar, "palette").accelerator).toBe("Cmd+Shift+KeyP");
    expect(item(bar, "queue").accelerator).toBe("Cmd+KeyB");
    expect(item(bar, "broadcast").accelerator).toBe("Cmd+Shift+KeyB");
    expect(item(bar, "layout-editor").accelerator).toBe("Cmd+Shift+KeyG");
    expect(item(bar, "zoom-in").accelerator).toBe("Cmd+Equal");
    expect(item(bar, "zoom-out").accelerator).toBe("Cmd+Minus");
    expect(item(bar, "zoom-reset").accelerator).toBe("Cmd+Digit0");
    expect(item(bar, "new-mission")).toMatchObject({ accelerator: "Cmd+Shift+KeyM", shortcut: "new-mission" });
    expect(item(bar, "mission-list")).toMatchObject({ accelerator: null, shortcut: null });
    expect(item(bar, "resume-all")).toMatchObject({ accelerator: "Cmd+Shift+KeyR", shortcut: "resume-all" });
    // 메뉴만 가진 조합은 페이지 단축키가 아니다(되울림 거르기 대상이 아니다).
    expect(item(bar, "settings")).toMatchObject({ accelerator: "CmdOrCtrl+,", shortcut: null });
    expect(item(bar, "quit")).toMatchObject({ accelerator: "CmdOrCtrl+Q", shortcut: null });
    // 붙여넣기 조합은 편집 메뉴의 시스템 항목(네이티브 붙여넣기 경로)이 갖는다.
    expect(item(bar, "paste-terminal")).toMatchObject({ accelerator: null, shortcut: null });
  });

  it("follows user overrides and drops pass-through combos", () => {
    const bar = menu(
      {},
      { overrides: { search: { code: "KeyG", ctrl: true, meta: false, shift: true }, "split-row": "pass" } },
    );
    expect(item(bar, "find").accelerator).toBe("Ctrl+Shift+KeyG");
    expect(item(bar, "split-row").accelerator).toBeNull();
    expect(item(bar, "split-row").shortcut).toBe("split-row");
  });

  it("registers only combos the menu bar can draw", () => {
    expect(menuAccelerator({ code: "IntlBackslash", ctrl: false, meta: true, shift: false }, "darwin")).toBeNull();
    expect(menuAccelerator({ code: "KeyD", ctrl: false, meta: false, shift: true }, "darwin")).toBeNull();
    expect(menuAccelerator({ code: "F5", ctrl: true, meta: true, shift: false }, "darwin")).toBe("Cmd+Ctrl+F5");
    expect(menuAccelerator({ code: "BracketLeft", ctrl: false, meta: true, shift: true }, "darwin")).toBe(
      "Cmd+Shift+BracketLeft",
    );
    expect(menuAccelerator(null, "darwin")).toBeNull();
  });

  it("with no tabs, disables what needs a pane, a tab or notifications but keeps ways to start", () => {
    const bar = menu();
    const disabled: NativeMenuCommand[] = [
      "close-pane",
      "close-tab",
      "close-all-tabs",
      "copy-selection",
      "paste-terminal",
      "select-all-terminal",
      "clear-terminal",
      "copy-cwd",
      "copy-resume",
      "notifications",
      "clear-notifications",
      "zoom-in",
      "zoom-out",
      "zoom-reset",
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
      "new-mission",
      "mission-list",
      "resume-all",
    ];
    for (const command of disabled) expect(item(bar, command).enabled, command).toBe(false);
    const enabled = NATIVE_MENU_COMMANDS.filter((command) => !disabled.includes(command));
    for (const command of enabled) expect(item(bar, command).enabled, command).toBe(true);
  });

  it("enables resume-all only while a live workload is suspended", () => {
    const base = {
      workloads: [workload("w-run"), workload("w-sus", { guard: suspendedGuard })],
    };
    expect(item(menu(base), "resume-all").enabled).toBe(true);
    // 일시정지가 풀리거나, 끝난 작업에 남은 표시로는 대상이 아니다.
    expect(item(menu({ workloads: [workload("w-run")] }), "resume-all").enabled).toBe(false);
    expect(
      item(menu({ workloads: [workload("w-sus", { state: "INTERRUPTED", guard: suspendedGuard })] }), "resume-all")
        .enabled,
    ).toBe(false);
  });

  it("aims pane items at the focused pane of the tab being viewed, never a stale focus in another tab", () => {
    const tabs = [terminalTab("t1", ["a"]), terminalTab("t2", ["b", "c"])];
    const panes = { a: pane("a"), b: pane("b"), c: pane("c") };
    const stale = menu({ tabs, panes, activeTabId: "t2", focusedLeafId: "a" });
    // 창이 있는 terminal 탭인데 초점 pane이 없으면 새 창을 둘 자리가 없다(분할·새 터미널 모두).
    for (const command of ["close-pane", "zoom-in", "split-row", "new-terminal", "paste-terminal", "select-all-terminal"] as const) {
      expect(item(stale, command).enabled, command).toBe(false);
    }
    const focused = menu({ tabs, panes, activeTabId: "t2", focusedLeafId: "b" });
    for (const command of [
      "close-pane",
      "split-row",
      "new-terminal",
      "detach-pane",
      "move-pane",
      "copy-cwd",
      "zoom-in",
      "paste-terminal",
      "select-all-terminal",
    ] as const) {
      expect(item(focused, command).enabled, command).toBe(true);
    }
  });

  it("follows the focused pane's phase, values and tab cap", () => {
    const base = { tabs: [terminalTab("t1", ["a"])], activeTabId: "t1", focusedLeafId: "a" };
    expect(item(menu({ ...base, panes: { a: pane("a", { phase: "replaying" }) } }), "clear-terminal").enabled).toBe(false);
    const exited = menu({ ...base, panes: { a: pane("a", { phase: "exited", cwd: null }) } });
    expect(item(exited, "restart-pane").enabled).toBe(true);
    expect(item(exited, "clear-terminal").enabled).toBe(true);
    expect(item(exited, "copy-cwd").enabled).toBe(false);
    expect(item(exited, "detach-pane").enabled).toBe(false);
    expect(item(exited, "move-pane").enabled).toBe(false);
    const resumable = menu({
      ...base,
      panes: { a: pane("a", { resume: { agent: "claude", agentSessionId: "abc" } as PaneMeta["resume"] }) },
    });
    expect(item(resumable, "copy-resume").enabled).toBe(true);
    const full = menu(fullTab);
    expect(item(full, "split-row").enabled).toBe(false);
    expect(item(full, "split-column").enabled).toBe(false);
    expect(item(full, "new-terminal").enabled).toBe(false);
    expect(item(full, "detach-pane").enabled).toBe(true);
  });

  it("reads terminal state that is not in the store: selection, font size bounds, live input", () => {
    const base = { tabs: [terminalTab("t1", ["a"])], activeTabId: "t1", focusedLeafId: "a", panes: { a: pane("a") } };
    const zoom = (bar: NativeMenuSubmenuSpec[]) =>
      (["zoom-in", "zoom-out", "zoom-reset"] as const).map((command) => item(bar, command).enabled);
    expect(zoom(menu(base))).toEqual([true, true, false]);
    expect(zoom(menu(base, { paneFontSize: () => MAX_FONT_SIZE }))).toEqual([false, true, true]);
    expect(zoom(menu(base, { paneFontSize: () => MIN_FONT_SIZE }))).toEqual([true, false, true]);
    expect(item(menu(base), "copy-selection").enabled).toBe(false);
    expect(item(menu(base, { paneHasSelection: (leafId) => leafId === "a" }), "copy-selection").enabled).toBe(true);
    const replaying = menu({ ...base, panes: { a: pane("a", { phase: "replaying" }) } });
    expect(item(replaying, "paste-terminal").enabled).toBe(false);
    expect(item(replaying, "select-all-terminal").enabled).toBe(true);
  });

  it("스냅샷은 글자 크기 값이 아니라 경계 판정만 담는다 — 줌 한 단계로 메뉴를 다시 만들지 않게", () => {
    // nativeMenu는 스냅샷을 JSON으로 직렬화한 키가 같으면 메뉴를 다시 만들지
    // 않는다. 글자 크기 값을 그대로 담으면 줌 한 단계마다 키가 달라져 ~100노드
    // 트리를 매번 새로 만든다 — 실제로 메뉴가 쓰는 것은 경계 셋뿐이다.
    const base = { tabs: [terminalTab("t1", ["a"])], activeTabId: "t1", focusedLeafId: "a", panes: { a: pane("a") } };
    const key = (fontSize: number): string => JSON.stringify(snapshotOf(base, { paneFontSize: () => fontSize }));

    // 기본 크기를 벗어나는 첫 단계에서만 달라진다(zoom-reset이 켜진다).
    expect(key(BASE_FONT + 1)).not.toBe(key(BASE_FONT));
    // 그 뒤의 평범한 단계들은 같은 키다 — 조기 return이 살아 있다.
    expect(key(BASE_FONT + 2)).toBe(key(BASE_FONT + 1));
    expect(key(BASE_FONT + 3)).toBe(key(BASE_FONT + 1));
    // 상·하한에 닿을 때만 다시 달라진다(zoom-in/zoom-out이 꺼진다).
    expect(key(MAX_FONT_SIZE)).not.toBe(key(BASE_FONT + 1));
    expect(key(MIN_FONT_SIZE)).not.toBe(key(BASE_FONT + 1));
  });

  it("follows the active tab's position and kind", () => {
    const tabs: TabState[] = [
      terminalTab("t1", ["a"]),
      { kind: "mission", id: "m1", title: "mission", missionId: "mission-1" },
      terminalTab("t2", ["b"]),
    ];
    const first = menu({ tabs, activeTabId: "t1" });
    expect(item(first, "move-tab-left").enabled).toBe(false);
    expect(item(first, "move-tab-right").enabled).toBe(true);
    expect(item(first, "rename-tab").enabled).toBe(true);
    expect(item(first, "merge-tab").enabled).toBe(true);
    expect(item(first, "regroup").enabled).toBe(true);
    const mission = menu({ tabs, activeTabId: "m1" });
    expect(item(mission, "rename-tab").enabled).toBe(false);
    expect(item(mission, "merge-tab").enabled).toBe(false);
    // mission 탭에서의 분할·새 터미널은 새 terminal 탭의 첫 pane이 된다.
    expect(item(mission, "split-row").enabled).toBe(true);
    expect(item(mission, "new-terminal").enabled).toBe(true);
    expect(item(menu({ tabs, activeTabId: "t2" }), "move-tab-right").enabled).toBe(false);
  });

  it("탭 순환 항목은 탭이 둘 이상일 때만 켜지고 §3 표의 조합을 단다", () => {
    const one = menu({ tabs: [terminalTab("t1", ["a"])], activeTabId: "t1", panes: { a: pane("a") } });
    expect(item(one, "next-tab").enabled).toBe(false);
    expect(item(one, "prev-tab").enabled).toBe(false);
    const two = menu({ tabs: [terminalTab("t1", ["a"]), terminalTab("t2", ["b"])], activeTabId: "t1" });
    expect(item(two, "next-tab")).toMatchObject({
      accelerator: "Cmd+Shift+BracketRight",
      shortcut: "next-tab",
      enabled: true,
    });
    expect(item(two, "prev-tab")).toMatchObject({
      accelerator: "Cmd+Shift+BracketLeft",
      shortcut: "prev-tab",
      enabled: true,
    });
    // mission 탭에서도 탭 순환은 된다(terminal 탭 전용이 아니다).
    const mission = menu({
      tabs: [terminalTab("t1", ["a"]), { kind: "mission", id: "m1", title: "m", missionId: "mission-1" }],
      activeTabId: "m1",
    });
    expect(item(mission, "next-tab").enabled).toBe(true);
  });

  it("waits while a dialog is open: only About, Quit and the language stay enabled", () => {
    const bar = menu({ modal: { kind: "palette" } }, { shells: [zsh], notifications: { count: 1, open: false } });
    for (const entry of menuItems(bar)) {
      const open =
        entry.target.kind === "language" ||
        (entry.target.kind === "command" && (entry.target.command === "about" || entry.target.command === "quit"));
      expect(entry.enabled, entry.id).toBe(open);
    }
    const submenus = bar[1]?.items.filter((entry) => entry.kind === "submenu") ?? [];
    expect(submenus.map((entry) => entry.kind === "submenu" && entry.enabled)).toEqual([false, false]);
  });

  it("mirrors state in check marks", () => {
    const bar = menu(
      { page: "layout", queueDrawerOpen: true, graphDrawerOpen: false, broadcastInput: true },
      { language: "en", notifications: { count: 1, open: true } },
    );
    expect(item(bar, "layout-editor").checked).toBe(true);
    expect(item(bar, "queue").checked).toBe(true);
    expect(item(bar, "graphs").checked).toBe(false);
    expect(item(bar, "notifications").checked).toBe(true);
    expect(item(bar, "broadcast").checked).toBe(true);
    expect(item(bar, "find").checked).toBeUndefined();
    const languages = menuItems(bar).filter((entry) => entry.target.kind === "language");
    expect(languages.map((entry) => [entry.label, entry.checked])).toEqual([
      ["한국어", false],
      ["English", true],
    ]);
  });

  it("offers the notification panel while there are notifications or it is open", () => {
    const some = menu({}, { notifications: { count: 2, open: false } });
    expect(item(some, "notifications")).toMatchObject({ enabled: true, checked: false });
    expect(item(some, "clear-notifications").enabled).toBe(true);
    const openEmpty = menu({}, { notifications: { count: 0, open: true } });
    expect(item(openEmpty, "notifications")).toMatchObject({ enabled: true, checked: true });
    expect(item(openEmpty, "clear-notifications").enabled).toBe(false);
  });

  it("offers a new AI mission only when the daemon speaks the mission protocol", () => {
    expect(item(menu({ missionProtocol: null }), "new-mission").enabled).toBe(false);
    expect(item(menu({ missionProtocol: 1 }), "new-mission").enabled).toBe(missionsAvailable({ mission_protocol: 1 }));
    expect(item(menu({ missionProtocol: 1 }), "mission-list").enabled).toBe(true);
    // 데몬이 앱보다 새로우면 숨기지 않고 비활성(앱 업데이트 안내는 상단 버튼·팔레트가 한다).
    expect(item(menu({ missionProtocol: 99 }, { productionBuild: true }), "new-mission").enabled).toBe(false);
  });

  it("puts AI mission items in the File menu right after New Tab", () => {
    const file = menu({ missionProtocol: 1 })[1];
    const ids = (file?.items ?? []).flatMap((entry) => (entry.kind === "item" ? [entry.id] : []));
    const at = ids.indexOf(nativeMenuItemId("new-tab"));
    expect(ids.slice(at, at + 3)).toEqual([
      nativeMenuItemId("new-tab"),
      nativeMenuItemId("new-mission"),
      nativeMenuItemId("mission-list"),
    ]);
    expect(item(menu({ missionProtocol: 1 }), "mission-list").label).toBe("AI 작업 목록…");
  });

  it("hides AI mission items in a production build when the daemon declares no protocol (development keeps them disabled)", () => {
    const hidden = menuItems(menu({ missionProtocol: null }, { productionBuild: true })).map((entry) => entry.id);
    expect(hidden).not.toContain(nativeMenuItemId("new-mission"));
    expect(hidden).not.toContain(nativeMenuItemId("mission-list"));
    const development = menuItems(menu({ missionProtocol: null }, { productionBuild: false })).map((entry) => entry.id);
    expect(development).toContain(nativeMenuItemId("new-mission"));
    expect(development).toContain(nativeMenuItemId("mission-list"));
    const ready = menuItems(menu({ missionProtocol: 1 }, { productionBuild: true })).map((entry) => entry.id);
    expect(ready).toContain(nativeMenuItemId("new-mission"));
  });

  it("lists shells to start a terminal with and to make the default", () => {
    const shells: NativeMenuShell[] = [
      { id: "zsh", label: "zsh", isDefault: false },
      { id: "fish", label: "Fish", isDefault: true },
    ];
    const submenus = (bar: NativeMenuSubmenuSpec[]): NativeMenuSubmenuSpec[] =>
      (bar[1]?.items ?? []).filter((entry): entry is NativeMenuSubmenuSpec => entry.kind === "submenu");
    const [start, defaults] = submenus(menu({}, { shells }));
    expect(start).toMatchObject({ id: "iyagi-menu-shells", enabled: true });
    expect(start?.items.map((entry) => (entry.kind === "item" ? [entry.id, entry.label] : null))).toEqual([
      ["iyagi-shell:zsh", "zsh"],
      ["iyagi-shell:fish", "Fish"],
    ]);
    expect(defaults).toMatchObject({ id: "iyagi-menu-default-shell", enabled: true });
    expect(defaults?.items.map((entry) => (entry.kind === "item" ? [entry.id, entry.checked] : null))).toEqual([
      ["iyagi-default-shell:zsh", false],
      ["iyagi-default-shell:fish", true],
    ]);
    // 셸 목록이 없으면(탐지 전) 둘 다 비활성이다.
    expect(submenus(menu()).map((submenu) => [submenu.enabled, submenu.items.length])).toEqual([
      [false, 0],
      [false, 0],
    ]);
    // 탭이 가득 차 새 터미널을 열 수 없으면 셸 목록은 막고, 기본 셸 고르기는 그대로 둔다.
    expect(submenus(menu(fullTab, { shells })).map((submenu) => submenu.enabled)).toEqual([false, true]);
  });
});
