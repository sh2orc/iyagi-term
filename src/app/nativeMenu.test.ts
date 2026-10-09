/**
 * macOS 메뉴 막대 네이티브 반영(04-ui §3-2): 메뉴는 한 번만 만들고, 언어·상태·단축키 재정의·셸 목록·
 * 알림이 바뀌거나 입력이 끝나면 달라진 항목만 고친다. 종료·정보는 앱 흐름을 부르고, 페이지가 흘려보낸
 * 단축키의 되울림은 실행하지 않는다. 실패는 한 번만 알리고 같은 상태로 되풀이하지 않으며, 만들다 만
 * 항목은 닫고, 셸 목록을 다시 만들다 실패해도 옛 목록을 지킨다.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { useI18nStore } from "../i18n";
import { useNotificationStore, type NotificationItem } from "../features/notifications/notificationStore";
import type { ShellProfile } from "../features/terminal/shellProfiles";
import type { ShortcutAction } from "../features/terminal/shortcuts";
import { makeLeaf } from "../features/terminal/splitTree";
import { MAX_FONT_SIZE } from "../features/terminal/zoom";
import { usePreferences } from "../store/preferences";
import { useWorkbenchStore, type PaneMeta } from "../store/workbenchStore";
import { useShellProfileStore } from "../stores/shellProfileStore";
import type { EchoKeyEvent } from "./menuKeyEcho";
import {
  ABOUT_MENU_ITEM_ID,
  createNativeMenu,
  QUIT_MENU_ITEM_ID,
  type NativeMenuApi,
  type NativeMenuOptions,
} from "./nativeMenu";

interface FakeNode {
  kind: "item" | "check" | "predefined" | "submenu";
  id?: string;
  item?: string;
  text: string;
  enabled: boolean;
  checked?: boolean;
  accelerator: string | null;
  action?: () => void;
  items: FakeNode[];
  closed: boolean;
}

interface TextHold {
  id: string;
  until: Promise<void>;
  reached: boolean;
}

/** 다시 만든 항목은 교체 번호가 붙은 네이티브 id(`<모델 id>#<n>`)를 받는다 — 시험은 모델 id로 찾는다. */
function baseId(id: string | undefined): string | undefined {
  return id?.split("#")[0];
}

function fakeApi() {
  const menuBars: FakeNode[][] = [];
  const created: FakeNode[] = [];
  const calls: Array<{ op: string; id: string | undefined; value: unknown }> = [];
  const control: {
    failItem: string | null;
    failAppendAt: number | null;
    failMenuBar: boolean;
    menuBarAttempts: number;
    holdText: TextHold | null;
  } = {
    failItem: null,
    failAppendAt: null,
    failMenuBar: false,
    menuBarAttempts: 0,
    holdText: null,
  };
  const make = (patch: Partial<FakeNode> & Pick<FakeNode, "kind">): FakeNode => {
    const node: FakeNode = { text: "", enabled: true, accelerator: null, items: [], closed: false, ...patch };
    created.push(node);
    return node;
  };
  // Tauri는 닫힌 자원 id를 거절한다 — 가짜도 닫힌 항목과 붙지 않은 자식을 받지 않는다.
  const alive = (target: FakeNode): void => {
    if (target.closed) throw new Error(`closed native resource: ${target.id ?? target.item ?? target.kind}`);
  };
  const refuse = (id: string): void => {
    if (control.failItem !== baseId(id)) return;
    control.failItem = null;
    throw new Error(`refused to create ${id}`);
  };
  const api: NativeMenuApi<FakeNode> = {
    version: async () => "1.2.3",
    item: async (o) => {
      refuse(o.id);
      return make({ kind: "item", id: o.id, text: o.text, enabled: o.enabled, accelerator: o.accelerator ?? null, action: o.action });
    },
    checkItem: async (o) => {
      refuse(o.id);
      return make({
        kind: "check",
        id: o.id,
        text: o.text,
        enabled: o.enabled,
        accelerator: o.accelerator ?? null,
        action: o.action,
        checked: o.checked,
      });
    },
    predefined: async (item, text) => make({ kind: "predefined", item, text: text ?? "" }),
    submenu: async (o) => {
      o.items.forEach((child) => alive(child));
      return make({ kind: "submenu", id: o.id, text: o.text, enabled: o.enabled, items: [...o.items] });
    },
    menuBar: async (items) => {
      control.menuBarAttempts += 1;
      if (control.failMenuBar) throw new Error("menu bar refused");
      items.forEach((item) => alive(item));
      menuBars.push(items);
    },
    setText: async (target, text) => {
      const hold = control.holdText;
      if (hold && hold.id === target.id) {
        hold.reached = true;
        await hold.until;
      }
      alive(target);
      calls.push({ op: "setText", id: target.id, value: text });
      target.text = text;
    },
    setEnabled: async (target, enabled) => {
      alive(target);
      calls.push({ op: "setEnabled", id: target.id, value: enabled });
      target.enabled = enabled;
    },
    setChecked: async (target, checked) => {
      alive(target);
      calls.push({ op: "setChecked", id: target.id, value: checked });
      target.checked = checked;
    },
    setAccelerator: async (target, accelerator) => {
      alive(target);
      calls.push({ op: "setAccelerator", id: target.id, value: accelerator });
      target.accelerator = accelerator;
    },
    append: async (submenu, items) => {
      alive(submenu);
      // Tauri는 하나씩 붙이다 중간에 실패할 수 있다.
      for (let index = 0; index < items.length; index += 1) {
        if (control.failAppendAt === index) {
          control.failAppendAt = null;
          throw new Error("append refused");
        }
        const item = items[index];
        if (!item) continue;
        alive(item);
        submenu.items.push(item);
      }
    },
    remove: async (submenu, item) => {
      alive(submenu);
      // muda는 떼어 낼 자식을 id로 찾는다(먼저 붙은 것) — 같은 id가 둘이면 엉뚱한 항목이 떨어진다.
      const index = submenu.items.findIndex((child) => (item.id === undefined ? child === item : child.id === item.id));
      if (index < 0) throw new Error("not a child of this submenu");
      submenu.items.splice(index, 1);
    },
    close: async (target) => {
      alive(target);
      target.closed = true;
    },
  };
  const find = (id: string, nodes: FakeNode[] = menuBars[menuBars.length - 1] ?? []): FakeNode | undefined => {
    for (const candidate of nodes) {
      if (candidate.id !== undefined && baseId(candidate.id) === id) return candidate;
      const found = find(id, candidate.items);
      if (found) return found;
    }
    return undefined;
  };
  /** submenu 자식의 모델 id(교체 번호를 뗀 것). */
  const ids = (submenuId: string) => find(submenuId)?.items.map((node) => baseId(node.id));
  return { api, menuBars, created, calls, control, find, ids };
}

/** 네이티브 체크 항목은 누르는 순간 스스로 표시를 뒤집는다(muda). */
function click(node: FakeNode | undefined): void {
  if (!node) throw new Error("no such menu item");
  if (node.kind === "check") node.checked = !node.checked;
  node.action?.();
}

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
    paneFontSize: vi.fn((_leafId: string) => usePreferences.getState().baseFontSize),
    paneHasSelection: vi.fn((_leafId: string) => false),
  };
}

const zsh: ShellProfile = {
  id: "zsh",
  label: "zsh",
  labelKey: null,
  program: "/bin/zsh",
  argv: ["-l"],
  kind: "unix",
  detail: "zsh",
  builtin: true,
};

const fish: ShellProfile = {
  id: "fish",
  label: "Fish",
  labelKey: null,
  program: "/opt/homebrew/bin/fish",
  argv: [],
  kind: "unix",
  detail: null,
  builtin: false,
};

const notification: NotificationItem = {
  id: "n1",
  kind: "workload-finished",
  title: "build finished",
  detail: null,
  at: "2026-09-14T00:00:00.000Z",
  sessionId: null,
  acknowledged: false,
};

function setup(overrides?: Partial<NativeMenuOptions>) {
  const fake = fakeApi();
  const nativeMenu = createNativeMenu(fake.api);
  const controller = fakeController();
  let keyListener: ((event: EchoKeyEvent) => void) | null = null;
  let settleListener: (() => void) | null = null;
  const options: NativeMenuOptions = {
    platform: "darwin",
    controller,
    onAbout: vi.fn(),
    onError: vi.fn(),
    keydowns: (listener) => {
      keyListener = listener;
      return () => {
        keyListener = null;
      };
    },
    settles: (listener) => {
      settleListener = listener;
      return () => {
        settleListener = null;
      };
    },
    detectShells: async () => [zsh],
  };
  const stop = nativeMenu.install({ ...options, ...overrides });
  const keydown = (patch: Partial<EchoKeyEvent>) =>
    keyListener?.({
      code: "KeyD",
      key: "d",
      ctrlKey: false,
      metaKey: true,
      altKey: false,
      shiftKey: false,
      defaultPrevented: false,
      ...patch,
    });
  const settle = () => settleListener?.();
  return { ...fake, nativeMenu, controller, options, stop, keydown, settle };
}

function focusOnePane(): void {
  useWorkbenchStore.setState({
    tabs: [{ kind: "terminal", id: "t1", title: "one", root: makeLeaf("a", "view-a") }],
    activeTabId: "t1",
    focusedLeafId: "a",
    panes: {
      a: { leafId: "a", viewId: "view-a", sessionId: null, title: "a", cwd: null, phase: "live", usage: null } as PaneMeta,
    },
  });
}

beforeEach(() => {
  useWorkbenchStore.setState({
    tabs: [],
    activeTabId: null,
    focusedLeafId: null,
    panes: {},
    modal: null,
    page: "terminal",
    settingsGroup: null,
    renamingTabId: null,
    broadcastInput: false,
    queueDrawerOpen: false,
    graphDrawerOpen: false,
    missionProtocol: null,
    toast: null,
  });
  useI18nStore.getState().setLanguage(null);
  usePreferences.setState({ shortcutOverrides: {} });
  useShellProfileStore.setState({ custom: [], defaultProfileId: null });
  useNotificationStore.setState({ items: [], panelOpen: false });
});

describe("native menu bar", () => {
  it("builds the whole menu bar once, in the UI language, even when installed twice", async () => {
    const menu = setup();
    // StrictMode: 설치 → 해제 → 다시 설치.
    menu.stop();
    const stop = menu.nativeMenu.install(menu.options);
    await menu.nativeMenu.flush();
    expect(menu.menuBars).toHaveLength(1);
    expect(menu.menuBars[0]?.map((submenu) => submenu.text)).toEqual([
      "IYAGI Term",
      "파일",
      "편집",
      "보기",
      "터미널",
      "탭",
      "창",
      "도움말",
    ]);
    stop();
  });

  it("keeps Quit on Cmd+Q and About as app-owned items that call the latest handlers", async () => {
    const menu = setup();
    await menu.nativeMenu.flush();
    expect(menu.find(QUIT_MENU_ITEM_ID)).toMatchObject({
      kind: "item",
      id: QUIT_MENU_ITEM_ID,
      accelerator: "CmdOrCtrl+Q",
      text: "IYAGI 종료",
    });
    expect(menu.find(ABOUT_MENU_ITEM_ID)).toMatchObject({ kind: "item", id: ABOUT_MENU_ITEM_ID, text: "IYAGI 정보" });
    // 컨트롤러가 바뀌면(Workbench 재생성) 메뉴는 그대로 두고 새 처리기를 부른다.
    menu.stop();
    const controller = fakeController();
    const onAbout = vi.fn();
    menu.nativeMenu.install({ ...menu.options, controller, onAbout });
    await menu.nativeMenu.flush();
    click(menu.find(QUIT_MENU_ITEM_ID));
    click(menu.find(ABOUT_MENU_ITEM_ID));
    expect(menu.controller.requestQuit).not.toHaveBeenCalled();
    expect(controller.requestQuit).toHaveBeenCalledOnce();
    expect(onAbout).toHaveBeenCalledWith("1.2.3");
    expect(menu.menuBars).toHaveLength(1);
  });

  it("relabels in place when the language changes", async () => {
    const menu = setup();
    await menu.nativeMenu.flush();
    useI18nStore.getState().setLanguage("en");
    await menu.nativeMenu.flush();
    expect(menu.menuBars).toHaveLength(1);
    expect(menu.menuBars[0]?.map((submenu) => submenu.text)).toEqual([
      "IYAGI Term",
      "File",
      "Edit",
      "View",
      "Terminal",
      "Tab",
      "Window",
      "Help",
    ]);
    expect(menu.find("iyagi-split-row")?.text).toBe("Split Right");
    expect(menu.find(QUIT_MENU_ITEM_ID)?.text).toBe("Quit IYAGI");
    expect(menu.find("iyagi-language-en")?.checked).toBe(true);
    expect(menu.find("iyagi-language-ko")?.checked).toBe(false);
  });

  it("follows workbench state and touches only the items that changed", async () => {
    const menu = setup();
    await menu.nativeMenu.flush();
    menu.calls.length = 0;
    useWorkbenchStore.setState({ broadcastInput: true });
    await menu.nativeMenu.flush();
    expect(menu.calls).toEqual([{ op: "setChecked", id: "iyagi-broadcast", value: true }]);
    // 메뉴에 닿지 않는 상태 변화는 네이티브를 건드리지 않는다.
    menu.calls.length = 0;
    useWorkbenchStore.setState({ toast: "hello" });
    await menu.nativeMenu.flush();
    expect(menu.calls).toEqual([]);
    useWorkbenchStore.setState({ modal: { kind: "palette" } });
    await menu.nativeMenu.flush();
    expect(menu.find("iyagi-split-row")?.enabled).toBe(false);
    expect(menu.find(QUIT_MENU_ITEM_ID)?.enabled).toBe(true);
  });

  it("shows the user's shortcut overrides and drops pass-through combos", async () => {
    const menu = setup();
    await menu.nativeMenu.flush();
    expect(menu.find("iyagi-find")?.accelerator).toBe("Cmd+KeyF");
    usePreferences.setState({ shortcutOverrides: { search: { code: "KeyG", ctrl: true, meta: false, shift: true } } });
    await menu.nativeMenu.flush();
    expect(menu.find("iyagi-find")?.accelerator).toBe("Ctrl+Shift+KeyG");
    usePreferences.setState({ shortcutOverrides: { search: "pass" } });
    await menu.nativeMenu.flush();
    expect(menu.find("iyagi-find")?.accelerator).toBeNull();
  });

  it("does not re-run a shortcut the page declined, on any keyboard layout, but runs a click", async () => {
    const menu = setup();
    await menu.nativeMenu.flush();
    // 페이지가 가드(예: 길게 누름)로 흘려보낸 Cmd+D를 WebKit이 메뉴로 다시 보냈다.
    menu.keydown({ code: "KeyD", key: "d" });
    click(menu.find("iyagi-split-row"));
    expect(menu.controller.dispatchShortcut).not.toHaveBeenCalled();
    click(menu.find("iyagi-split-row"));
    expect(menu.controller.dispatchShortcut).toHaveBeenCalledWith("split-row");
    // Dvorak: D라고 적힌 키는 물리 KeyH다 — AppKit은 글자 "d"로 메뉴를 맞추지만 페이지가 흘려보낸 조합이다.
    menu.controller.dispatchShortcut.mockClear();
    menu.keydown({ code: "KeyH", key: "d" });
    click(menu.find("iyagi-split-row"));
    // 독일어: "="는 Shift+0이다 — ⌘= 항목이 불렸어도 페이지가 흘려보낸 조합이다.
    menu.keydown({ code: "Digit0", key: "=", shiftKey: true });
    click(menu.find("iyagi-zoom-in"));
    expect(menu.controller.dispatchShortcut).not.toHaveBeenCalled();
    // 페이지가 처리한 keydown은 되울림이 아니다 — 그 뒤의 클릭은 실행한다.
    menu.keydown({ code: "KeyP", key: "P", shiftKey: true, defaultPrevented: true });
    click(menu.find("iyagi-palette"));
    expect(menu.controller.dispatchShortcut).toHaveBeenLastCalledWith("palette");
    // 메뉴만 가진 조합(종료)은 되울림 거르기 대상이 아니다.
    menu.keydown({ code: "KeyQ", key: "q" });
    click(menu.find(QUIT_MENU_ITEM_ID));
    expect(menu.controller.requestQuit).toHaveBeenCalledOnce();
  });

  it("puts a check mark back to the state after the native toggle", async () => {
    const menu = setup();
    await menu.nativeMenu.flush();
    const broadcast = menu.find("iyagi-broadcast");
    // 가짜 컨트롤러는 상태를 바꾸지 않는다 — 네이티브만 체크가 뒤집힌 상태다.
    click(broadcast);
    expect(menu.controller.toggleBroadcast).toHaveBeenCalledOnce();
    expect(broadcast?.checked).toBe(true);
    await menu.nativeMenu.flush();
    expect(broadcast?.checked).toBe(false);
  });

  it("puts a check mark back even when the click lands while an update is in flight", async () => {
    const menu = setup();
    await menu.nativeMenu.flush();
    let release: () => void = () => undefined;
    const hold: TextHold = {
      // 터미널 메뉴(동시 입력)를 이미 지나 탭 메뉴 이름을 바꾸는 사이에 붙잡는다.
      id: "iyagi-menu-tab",
      until: new Promise<void>((resolve) => {
        release = resolve;
      }),
      reached: false,
    };
    menu.control.holdText = hold;
    useI18nStore.getState().setLanguage("en");
    for (let i = 0; i < 5000 && !hold.reached; i += 1) await Promise.resolve();
    expect(hold.reached).toBe(true);
    const broadcast = menu.find("iyagi-broadcast");
    click(broadcast);
    expect(broadcast?.checked).toBe(true);
    menu.control.holdText = null;
    release();
    await menu.nativeMenu.flush();
    expect(broadcast?.checked).toBe(false);
    expect(menu.find("iyagi-menu-tab")?.text).toBe("Tab");
  });

  it("lists detected and custom shells, rebuilds the lists when profiles change, and sets the default", async () => {
    const menu = setup();
    await menu.nativeMenu.flush();
    expect(menu.ids("iyagi-menu-shells")).toEqual(["iyagi-shell:zsh"]);
    expect(menu.find("iyagi-menu-shells")?.enabled).toBe(true);
    // 기본 셸을 아직 고르지 않았다 — 셸 선택기처럼 아무것도 표시하지 않는다.
    expect(menu.find("iyagi-default-shell:zsh")?.checked).toBe(false);
    const previous = menu.find("iyagi-shell:zsh");
    useShellProfileStore.setState({ custom: [fish] });
    await menu.nativeMenu.flush();
    expect(menu.ids("iyagi-menu-shells")).toEqual(["iyagi-shell:zsh", "iyagi-shell:fish"]);
    expect(previous?.closed).toBe(true);
    // 다시 만든 항목은 교체만의 네이티브 id를 받는다(옛 항목과 id가 겹치지 않는다).
    expect(menu.find("iyagi-shell:zsh")?.id).not.toBe("iyagi-shell:zsh");
    click(menu.find("iyagi-shell:fish"));
    expect(menu.controller.newTerminal).toHaveBeenCalledWith({
      program: "/opt/homebrew/bin/fish",
      argv: [],
      label: "Fish",
    });
    click(menu.find("iyagi-default-shell:fish"));
    expect(useShellProfileStore.getState().defaultProfileId).toBe("fish");
    await menu.nativeMenu.flush();
    expect(menu.find("iyagi-default-shell:fish")?.checked).toBe(true);
    expect(menu.find("iyagi-default-shell:zsh")?.checked).toBe(false);
  });

  it("keeps the old shell list when creating the new one fails, and recovers on the next change", async () => {
    const menu = setup();
    await menu.nativeMenu.flush();
    const oldZsh = menu.find("iyagi-shell:zsh");
    menu.control.failItem = "iyagi-shell:fish";
    useShellProfileStore.setState({ custom: [fish] });
    await menu.nativeMenu.flush();
    expect(menu.options.onError).toHaveBeenCalledOnce();
    const shells = menu.find("iyagi-menu-shells");
    expect(shells?.items).toHaveLength(1);
    expect(shells?.items[0]).toBe(oldZsh);
    expect(oldZsh?.closed).toBe(false);
    // 다음 변화에서 다시 만들고, 다른 항목 반영도 멈추지 않는다.
    useWorkbenchStore.setState({ broadcastInput: true });
    await menu.nativeMenu.flush();
    expect(menu.ids("iyagi-menu-shells")).toEqual(["iyagi-shell:zsh", "iyagi-shell:fish"]);
    expect(oldZsh?.closed).toBe(true);
    expect(menu.find("iyagi-broadcast")?.checked).toBe(true);
    expect(menu.find("iyagi-default-shell:fish")).toBeDefined();
    expect(menu.options.onError).toHaveBeenCalledOnce();
  });

  it("rolls back a partly failed append without detaching old items that share an id", async () => {
    const menu = setup();
    await menu.nativeMenu.flush();
    const oldZsh = menu.find("iyagi-shell:zsh");
    // Tauri가 새 목록을 하나씩 붙이다 두 번째에서 실패했다 — 되돌리기가 id로 떼어도 옛 zsh는 남아야 한다.
    menu.control.failAppendAt = 1;
    useShellProfileStore.setState({ custom: [fish] });
    await menu.nativeMenu.flush();
    expect(menu.options.onError).toHaveBeenCalledOnce();
    const shells = menu.find("iyagi-menu-shells");
    expect(shells?.items).toHaveLength(1);
    expect(shells?.items[0]).toBe(oldZsh);
    expect(oldZsh?.closed).toBe(false);
    useWorkbenchStore.setState({ broadcastInput: true });
    await menu.nativeMenu.flush();
    expect(menu.ids("iyagi-menu-shells")).toEqual(["iyagi-shell:zsh", "iyagi-shell:fish"]);
    expect(menu.find("iyagi-menu-shells")?.items.every((node) => !node.closed)).toBe(true);
    expect(oldZsh?.closed).toBe(true);
  });

  it("re-reads terminal state that is not in the store once input settles", async () => {
    focusOnePane();
    const menu = setup();
    await menu.nativeMenu.flush();
    expect(menu.find("iyagi-zoom-in")?.enabled).toBe(true);
    expect(menu.find("iyagi-copy-selection")?.enabled).toBe(false);
    // 키보드로 확대(페이지가 처리)하고 끌어서 선택했다 — 스토어는 모른다.
    menu.controller.paneFontSize.mockReturnValue(MAX_FONT_SIZE);
    menu.controller.paneHasSelection.mockReturnValue(true);
    menu.settle();
    await menu.nativeMenu.flush();
    expect(menu.find("iyagi-zoom-in")?.enabled).toBe(false);
    expect(menu.find("iyagi-zoom-reset")?.enabled).toBe(true);
    expect(menu.find("iyagi-copy-selection")?.enabled).toBe(true);
  });

  it("opens the notification panel from the menu and follows its state", async () => {
    const menu = setup();
    await menu.nativeMenu.flush();
    expect(menu.find("iyagi-notifications")).toMatchObject({ enabled: false, checked: false });
    useNotificationStore.setState({ items: [notification] });
    await menu.nativeMenu.flush();
    expect(menu.find("iyagi-notifications")?.enabled).toBe(true);
    expect(menu.find("iyagi-clear-notifications")?.enabled).toBe(true);
    click(menu.find("iyagi-notifications"));
    expect(useNotificationStore.getState().panelOpen).toBe(true);
    await menu.nativeMenu.flush();
    expect(menu.find("iyagi-notifications")?.checked).toBe(true);
    click(menu.find("iyagi-clear-notifications"));
    await menu.nativeMenu.flush();
    expect(useNotificationStore.getState().items).toEqual([]);
    expect(menu.find("iyagi-clear-notifications")?.enabled).toBe(false);
    // 패널은 아직 열려 있으니 알림 항목은 닫을 수 있게 남는다.
    expect(menu.find("iyagi-notifications")).toMatchObject({ enabled: true, checked: true });
  });

  it("stops following state after uninstall but keeps the menu", async () => {
    const menu = setup();
    await menu.nativeMenu.flush();
    menu.stop();
    menu.calls.length = 0;
    useWorkbenchStore.setState({ broadcastInput: true });
    await menu.nativeMenu.flush();
    expect(menu.calls).toEqual([]);
    expect(menu.menuBars).toHaveLength(1);
  });

  it("reports a failed menu bar once, closes what it built, and retries only when the menu's state changes", async () => {
    const fake = fakeApi();
    fake.control.failMenuBar = true;
    const nativeMenu = createNativeMenu(fake.api);
    // Workbench처럼 실패 알림이 스토어(토스트)에 쓴다 — 그 쓰기가 반영을 다시 부르지만 같은 상태로는 되풀이하지 않는다.
    const onError = vi.fn(() => useWorkbenchStore.setState({ toast: "menu failed" }));
    nativeMenu.install({
      platform: "darwin",
      controller: fakeController(),
      onAbout: vi.fn(),
      onError,
      keydowns: () => () => undefined,
      settles: () => () => undefined,
      detectShells: async () => [],
    });
    await nativeMenu.flush();
    expect(onError).toHaveBeenCalledOnce();
    expect(fake.control.menuBarAttempts).toBe(1);
    expect(fake.created.length).toBeGreaterThan(0);
    expect(fake.created.every((node) => node.closed)).toBe(true);
    fake.control.failMenuBar = false;
    useWorkbenchStore.setState({ broadcastInput: true });
    await nativeMenu.flush();
    expect(fake.control.menuBarAttempts).toBe(2);
    expect(fake.menuBars).toHaveLength(1);
    expect(fake.find("iyagi-broadcast")?.checked).toBe(true);
    expect(onError).toHaveBeenCalledOnce();
  });

  it("routes Windows Exit through the app quit confirmation", async () => {
    // Windows에서도 같은 앱 메뉴가 설치된다 — OS가 넣는 미리 정의된 Exit 대신
    // 앱의 종료 항목이 종료 확인 흐름(controller.requestQuit)을 부른다.
    const menu = setup({ platform: "windows" });
    await menu.nativeMenu.flush();
    const exit = menu.find(QUIT_MENU_ITEM_ID);
    expect(exit).toMatchObject({ kind: "item", id: QUIT_MENU_ITEM_ID, text: "IYAGI 종료" });
    click(exit);
    expect(menu.controller.requestQuit).toHaveBeenCalledOnce();
  });
});
