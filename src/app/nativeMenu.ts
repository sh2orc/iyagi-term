/**
 * macOS 메뉴 막대(04-ui §3-2) — 네이티브 메뉴를 만들고 앱 상태를 따라 고친다.
 *
 * 무엇을 보일지는 nativeMenuModel(순수)이, 고른 항목의 실행은 nativeMenuCommands가 맡는다. 여기서는
 * 메뉴를 한 번만 만들고(StrictMode의 두 번 설치·컨트롤러 교체에도 같은 메뉴), 상태·언어·단축키
 * 재정의·셸 목록·알림이 바뀌거나 키·마우스를 뗄 때(스토어에 없는 선택 영역·글꼴 크기) 달라진 부분만
 * 네이티브 항목에 반영한다. 모양이 바뀐 submenu(셸 목록)는 새 자식을 붙인 뒤에 옛 자식을 뗀다.
 *
 * 실패는 한 번만 알리고 같은 상태로는 다시 시도하지 않는다 — 실패 알림(토스트)이 스토어를 바꿔 반영을
 * 다시 부르므로, 그렇지 않으면 실패가 끝없이 되풀이된다. 만들다 만 항목은 닫는다.
 *
 * 앱 단축키를 단 항목은 페이지가 흘려보낸 같은 조합의 되울림이면 실행하지 않는다(menuKeyEcho). 기본
 * Quit(`NSApp.terminate:`)은 Tauri의 ExitRequested를 건너뛰어 살아 있는 터미널을 어떻게 할지 물을
 * 틈이 없으므로, 같은 자리·같은 조합(Cmd+Q)의 앱 항목이 종료 확인 흐름(features/app/quit.ts)을
 * 부른다. 정보 항목은 넓은 앱 대화상자를 연다.
 */

import { CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu } from "@tauri-apps/api/menu";
import { getVersion } from "@tauri-apps/api/app";
import { resolveLanguage, translate, useI18nStore, type Language } from "../i18n";
import { useNotificationStore } from "../features/notifications/notificationStore";
import { detectShellProfiles } from "../features/terminal/shellDeps";
import { profileDisplayLabel, type ShellProfile } from "../features/terminal/shellProfiles";
import { effectiveBinding, type Platform } from "../features/terminal/shortcuts";
import { usePreferences } from "../store/preferences";
import { useWorkbenchStore } from "../store/workbenchStore";
import { useShellProfileStore } from "../stores/shellProfileStore";
import { createKeyEchoFilter, type EchoKeyEvent } from "./menuKeyEcho";
import { runNativeMenuCommand, type NativeMenuController } from "./nativeMenuCommands";
import {
  buildNativeMenu,
  nativeMenuItemId,
  nativeMenuSnapshot,
  type NativeMenuEntrySpec,
  type NativeMenuPredefined,
  type NativeMenuSubmenuSpec,
} from "./nativeMenuModel";

export const QUIT_MENU_ITEM_ID = nativeMenuItemId("quit");
export const ABOUT_MENU_ITEM_ID = nativeMenuItemId("about");

export interface NativeItemOptions {
  id: string;
  text: string;
  enabled: boolean;
  accelerator?: string;
  action: () => void;
}

/** 네이티브 메뉴 조작 — 기본은 Tauri, 시험은 가짜를 넣는다. 핸들은 이 API가 만든 값 그대로다. */
export interface NativeMenuApi<H> {
  version(): Promise<string>;
  item(options: NativeItemOptions): Promise<H>;
  checkItem(options: NativeItemOptions & { checked: boolean }): Promise<H>;
  predefined(item: NativeMenuPredefined | "Separator", text?: string): Promise<H>;
  submenu(options: { id: string; text: string; enabled: boolean; items: H[] }): Promise<H>;
  /** 맨 윗줄 submenu들로 앱 메뉴를 만들어 걸고, 앞의 메뉴(Tauri 기본 메뉴)를 닫는다. */
  menuBar(items: H[]): Promise<void>;
  setText(handle: H, text: string): Promise<void>;
  setEnabled(handle: H, enabled: boolean): Promise<void>;
  setChecked(handle: H, checked: boolean): Promise<void>;
  setAccelerator(handle: H, accelerator: string | null): Promise<void>;
  append(submenu: H, items: H[]): Promise<void>;
  /** 시스템(muda)은 떼어 낼 자식을 id로 찾는다(같은 id면 먼저 붙은 것). */
  remove(submenu: H, item: H): Promise<void>;
  close(handle: H): Promise<void>;
}

type TauriMenuEntry = MenuItem | CheckMenuItem | PredefinedMenuItem | Submenu;

const tauriMenuApi: NativeMenuApi<TauriMenuEntry> = {
  version: () => getVersion(),
  // Tauri는 읽을 수 없는 조합을 조용히 버리고 항목은 만든다 — 등록할 조합은 모델이 미리 거른다.
  item: (options) => MenuItem.new(options),
  checkItem: (options) => CheckMenuItem.new(options),
  predefined: (item, text) => PredefinedMenuItem.new(text === undefined ? { item } : { item, text }),
  submenu: (options) => Submenu.new(options),
  async menuBar(items) {
    const menu = await Menu.new({ items });
    let previous: Menu | null;
    try {
      previous = await menu.setAsAppMenu();
    } catch (error) {
      await menu.close().catch(() => undefined);
      throw error;
    }
    // 앞의 메뉴를 닫지 못해도 새 메뉴는 이미 걸렸다.
    await previous?.close().catch(() => undefined);
  },
  setText: (handle, text) => handle.setText(text),
  async setEnabled(handle, enabled) {
    if ("setEnabled" in handle) await handle.setEnabled(enabled);
  },
  async setChecked(handle, checked) {
    if (handle instanceof CheckMenuItem) await handle.setChecked(checked);
  },
  async setAccelerator(handle, accelerator) {
    if (handle instanceof MenuItem || handle instanceof CheckMenuItem) await handle.setAccelerator(accelerator);
  },
  async append(submenu, items) {
    if (submenu instanceof Submenu) await submenu.append(items);
  },
  async remove(submenu, item) {
    if (submenu instanceof Submenu) await submenu.remove(item);
  },
  close: (handle) => handle.close(),
};

/** keydown(capture) 구독 — 해제 함수를 돌려준다. */
export type KeydownSource = (listener: (event: EchoKeyEvent) => void) => () => void;

/** 키·마우스를 뗀 직후 알림 — 스토어에 없는 터미널 상태(선택 영역·글꼴 크기)를 다시 읽을 때. */
export type SettleSource = (listener: () => void) => () => void;

const windowKeydowns: KeydownSource = (listener) => {
  if (typeof window === "undefined") return () => undefined;
  const onKeyDown = (event: KeyboardEvent) => listener(event);
  window.addEventListener("keydown", onKeyDown, true);
  return () => window.removeEventListener("keydown", onKeyDown, true);
};

const windowSettles: SettleSource = (listener) => {
  if (typeof window === "undefined") return () => undefined;
  // 한 틱 늦춘다 — 터미널이 확대·선택을 먼저 끝낸다.
  const onSettle = () => {
    setTimeout(listener, 0);
  };
  window.addEventListener("keyup", onSettle);
  window.addEventListener("mouseup", onSettle);
  return () => {
    window.removeEventListener("keyup", onSettle);
    window.removeEventListener("mouseup", onSettle);
  };
};

export interface NativeMenuOptions {
  platform: Platform;
  controller: NativeMenuController;
  /** 정보 항목 — 런타임 버전과 함께 앱 대화상자를 연다. */
  onAbout(version: string): void;
  /** 메뉴를 만들거나 고치지 못했다(토스트) — 실패가 시작될 때 한 번만 불린다. */
  onError(): void;
  /** 기본은 window의 keydown(capture). */
  keydowns?: KeydownSource;
  /** 기본은 window의 keyup·mouseup. */
  settles?: SettleSource;
  /** 내장 셸 탐지(기본 detectShellProfiles). */
  detectShells?: (platform: Platform) => Promise<ShellProfile[]>;
}

export interface NativeMenuInstaller {
  /** 메뉴를 (처음이면 만들어) 앱 상태에 묶는다. 돌려준 함수는 구독만 푼다 — 메뉴는 남는다. */
  install(options: NativeMenuOptions): () => void;
  /** 예약된 반영이 모두 끝날 때까지 기다린다(시험용). */
  flush(): Promise<void>;
}

interface Applied {
  label?: string;
  enabled?: boolean;
  checked?: boolean;
  accelerator?: string | null;
}

interface MenuNode<H> {
  spec: NativeMenuEntrySpec;
  handle: H;
  /** 마지막으로 네이티브에 반영한 값 — 모르면 undefined라 다음 반영에서 다시 쓴다. */
  applied: Applied;
  children: MenuNode<H>[];
  /** 사용자가 체크 항목을 누를 때마다 오른다(네이티브가 스스로 체크를 뒤집었다). */
  checkResets: number;
}

export function createNativeMenu<H>(api: NativeMenuApi<H>): NativeMenuInstaller {
  const echo = createKeyEchoFilter();
  let options: NativeMenuOptions | null = null;
  let built: { version: string; roots: MenuNode<H>[] } | null = null;
  let pending: Promise<void> = Promise.resolve();
  let scheduled = false;
  /** 마지막으로 끝까지 반영한 상태 — 같으면 네이티브를 건드리지 않는다. */
  let appliedKey: string | null = null;
  /** 반영에 실패한 상태 — 같은 상태로는 다시 시도하지 않는다. */
  let failedKey: string | null = null;
  /** 실패를 이미 알렸는가(성공할 때까지 다시 알리지 않는다). */
  let failing = false;
  /** 항목을 고를 때마다 오른다 — 반영하는 사이 고른 항목이 있으면 그 반영을 최신으로 치지 않는다. */
  let generation = 0;
  /** submenu 자식을 통째로 바꾼 횟수 — 다시 만든 항목의 네이티브 id를 교체마다 달리한다. */
  let rebuilds = 0;
  let builtinShells: ShellProfile[] = [];
  let shellsPlatform: Platform | null = null;

  const currentLanguage = (): Language => resolveLanguage(useI18nStore.getState().language);

  /** 감지된 셸 + 사용자 프로필(같은 id는 앞의 것) — 셸 선택기와 같은 목록이다. */
  const shellProfiles = (): ShellProfile[] => {
    const seen = new Set<string>();
    return [...builtinShells, ...useShellProfileStore.getState().custom].filter((profile) => {
      if (seen.has(profile.id)) return false;
      seen.add(profile.id);
      return true;
    });
  };

  const shellLabel = (profile: ShellProfile, language: Language): string =>
    profileDisplayLabel(profile, (key, params) => translate(language, key, params));

  /** 모델 id로 항목을 찾는다(네이티브 id에는 교체 번호가 붙을 수 있다). */
  const findNode = (nodes: MenuNode<H>[], id: string): MenuNode<H> | null => {
    for (const node of nodes) {
      if (node.spec.kind === "item" && node.spec.id === id) return node;
      const found = findNode(node.children, id);
      if (found) return found;
    }
    return null;
  };

  // 반영은 한 줄로 세운다: 네이티브 호출이 섞이면 옛 값이 새 값을 덮는다. 몰린 변경은 한 번으로 모은다.
  const schedule = (): void => {
    if (scheduled) return;
    scheduled = true;
    pending = pending.then(run);
  };

  const activate = (id: string): void => {
    const node = built ? findNode(built.roots, id) : null;
    const spec = node?.spec;
    if (!node || !spec || spec.kind !== "item") return;
    // 체크 항목은 누르는 순간 네이티브가 스스로 표시를 뒤집는다 — 실행 여부와 상관없이 상태 값으로 다시 맞춘다.
    if (typeof spec.checked === "boolean") {
      node.applied.checked = undefined;
      node.checkResets += 1;
    }
    // 다음 반영은 같은 상태여도 끝까지 비교한다(끝난 반영의 기록은 지우고, 진행 중인 반영은 generation으로 막는다).
    generation += 1;
    appliedKey = null;
    // 사용자가 고른 것은 다시 시도할 기회다.
    failedKey = null;
    const current = options;
    try {
      if (!current) return;
      if (spec.shortcut) {
        const binding = effectiveBinding(spec.shortcut, current.platform, usePreferences.getState().shortcutOverrides);
        // 페이지가 가드(모달·IME·길게 누름·이름 편집·다른 화면)로 흘려보낸 단축키가 메뉴로 되돌아왔다.
        if (binding !== null && echo.consumeEcho(binding)) return;
      }
      runNativeMenuCommand(spec.target, {
        controller: current.controller,
        openAbout: () => current.onAbout(built?.version ?? ""),
        resolveShell: (profileId) => {
          const profile = shellProfiles().find((candidate) => candidate.id === profileId);
          if (!profile) return null;
          return { program: profile.program, argv: profile.argv, label: shellLabel(profile, currentLanguage()) };
        },
      });
    } finally {
      schedule();
    }
  };

  const closeTree = async (node: MenuNode<H>): Promise<void> => {
    for (const child of node.children) await closeTree(child);
    await api.close(node.handle).catch(() => undefined);
  };

  /**
   * 모델 항목 하나를 네이티브로 만든다. tag가 있으면(submenu 자식 교체) 네이티브 id에 교체 번호를 붙인다 —
   * 시스템은 자식을 id로 찾아 떼므로, 같은 id의 옛 항목과 새 항목이 함께 붙어 있는 동안 엉뚱한 쪽을 떼지
   * 않게 한다. 고른 항목은 모델 id로 찾으므로 동작은 같다.
   */
  const create = async (entry: NativeMenuEntrySpec, tag: number | null = null): Promise<MenuNode<H>> => {
    const nativeId = (id: string): string => (tag === null ? id : `${id}#${tag}`);
    switch (entry.kind) {
      case "separator":
        return { spec: entry, handle: await api.predefined("Separator"), applied: {}, children: [], checkResets: 0 };
      case "predefined":
        return {
          spec: entry,
          handle: await api.predefined(entry.item, entry.label),
          applied: { label: entry.label },
          children: [],
          checkResets: 0,
        };
      case "item": {
        const itemOptions: NativeItemOptions = {
          id: nativeId(entry.id),
          text: entry.label,
          enabled: entry.enabled,
          action: () => activate(entry.id),
          ...(entry.accelerator === null ? {} : { accelerator: entry.accelerator }),
        };
        const handle =
          typeof entry.checked === "boolean"
            ? await api.checkItem({ ...itemOptions, checked: entry.checked })
            : await api.item(itemOptions);
        return {
          spec: entry,
          handle,
          applied: { label: entry.label, enabled: entry.enabled, checked: entry.checked, accelerator: entry.accelerator },
          children: [],
          checkResets: 0,
        };
      }
      case "submenu": {
        const children: MenuNode<H>[] = [];
        try {
          for (const child of entry.items) children.push(await create(child, tag));
          const handle = await api.submenu({
            id: nativeId(entry.id),
            text: entry.label,
            enabled: entry.enabled,
            items: children.map((child) => child.handle),
          });
          return { spec: entry, handle, applied: { label: entry.label, enabled: entry.enabled }, children, checkResets: 0 };
        } catch (error) {
          // 붙을 곳이 없는 네이티브 항목을 남기지 않는다.
          for (const child of children) await closeTree(child);
          throw error;
        }
      }
    }
  };

  const sameShape = (nodes: MenuNode<H>[], entries: NativeMenuEntrySpec[]): boolean =>
    nodes.length === entries.length &&
    nodes.every((node, index) => {
      const entry = entries[index];
      if (node.spec.kind !== entry.kind || node.spec.id !== entry.id) return false;
      // 일반 항목과 체크 항목은 다른 네이티브 종류다.
      if (node.spec.kind === "item" && entry.kind === "item") {
        return (typeof node.spec.checked === "boolean") === (typeof entry.checked === "boolean");
      }
      return true;
    });

  const update = async (node: MenuNode<H>, entry: NativeMenuEntrySpec): Promise<void> => {
    node.spec = entry;
    if (entry.kind === "separator") return;
    if (node.applied.label !== entry.label) {
      await api.setText(node.handle, entry.label);
      node.applied.label = entry.label;
    }
    if (entry.kind === "predefined") return;
    if (node.applied.enabled !== entry.enabled) {
      await api.setEnabled(node.handle, entry.enabled);
      node.applied.enabled = entry.enabled;
    }
    if (entry.kind === "submenu") {
      node.children = await syncChildren(node, node.children, entry.items);
      return;
    }
    if (node.applied.accelerator !== entry.accelerator) {
      await api.setAccelerator(node.handle, entry.accelerator);
      node.applied.accelerator = entry.accelerator;
    }
    if (typeof entry.checked === "boolean" && node.applied.checked !== entry.checked) {
      const resets = node.checkResets;
      await api.setChecked(node.handle, entry.checked);
      // 기다리는 사이 사용자가 눌러 네이티브가 다시 뒤집혔으면 반영했다고 치지 않는다(다음 반영이 다시 맞춘다).
      if (node.checkResets === resets) node.applied.checked = entry.checked;
    }
  };

  const syncChildren = async (
    parent: MenuNode<H> | null,
    nodes: MenuNode<H>[],
    entries: NativeMenuEntrySpec[],
  ): Promise<MenuNode<H>[]> => {
    if (sameShape(nodes, entries)) {
      for (let index = 0; index < entries.length; index += 1) await update(nodes[index], entries[index]);
      return nodes;
    }
    // 모양이 바뀐 submenu(셸 목록)는 자식을 통째로 바꾼다. 메뉴 막대의 맨 윗줄은 늘 같다.
    if (parent === null) throw new Error("native menu bar layout changed");
    // 새 자식을 먼저 만들어 붙이고 옛 자식은 그 뒤에 뗀다 — 도중에 실패해도 메뉴가 닫힌 항목을 가리키지 않는다.
    // 새 자식은 이번 교체만의 네이티브 id를 받으므로, 되돌리기(새 자식 떼기)와 교체(옛 자식 떼기)가 id로
    // 찾아 떼어도 서로를 건드리지 않는다(Tauri는 하나씩 붙이다 중간에 실패할 수 있다).
    rebuilds += 1;
    const tag = rebuilds;
    const created: MenuNode<H>[] = [];
    try {
      for (const entry of entries) created.push(await create(entry, tag));
      if (created.length > 0) await api.append(parent.handle, created.map((node) => node.handle));
    } catch (error) {
      for (const node of created) {
        await api.remove(parent.handle, node.handle).catch(() => undefined);
        await closeTree(node);
      }
      throw error;
    }
    for (const node of nodes) {
      await api.remove(parent.handle, node.handle).catch(() => undefined);
      await closeTree(node);
    }
    return created;
  };

  const buildMenuBar = async (spec: NativeMenuSubmenuSpec[]): Promise<{ version: string; roots: MenuNode<H>[] }> => {
    const version = await api.version();
    const roots: MenuNode<H>[] = [];
    try {
      for (const entry of spec) roots.push(await create(entry));
      await api.menuBar(roots.map((root) => root.handle));
    } catch (error) {
      // 반쯤 만든 메뉴는 닫는다 — 다음 시도가 처음부터 다시 만든다.
      for (const root of roots) await closeTree(root);
      throw error;
    }
    return { version, roots };
  };

  async function run(): Promise<void> {
    scheduled = false;
    const current = options;
    if (!current) return;
    const startGeneration = generation;
    let key: string | null = null;
    try {
      const language = currentLanguage();
      const preferences = usePreferences.getState();
      const storedDefaultShell = useShellProfileStore.getState().defaultProfileId;
      const notifications = useNotificationStore.getState();
      const snapshot = nativeMenuSnapshot(useWorkbenchStore.getState(), {
        platform: current.platform,
        language,
        overrides: preferences.shortcutOverrides,
        baseFontSize: preferences.baseFontSize,
        // 기본 셸 체크는 셸 선택기의 "기본" 표시와 같다 — 저장된 기본값만 친다(플랫폼 관례로 고른 셸은 아니다).
        shells: shellProfiles().map((profile) => ({
          id: profile.id,
          label: shellLabel(profile, language),
          isDefault: profile.id === storedDefaultShell,
        })),
        notifications: { count: notifications.items.length, open: notifications.panelOpen },
        paneFontSize: (leafId) => current.controller.paneFontSize(leafId),
        paneHasSelection: (leafId) => current.controller.paneHasSelection(leafId),
      });
      // 워크벤치 상태는 자주 바뀐다(자원 사용량·출력 활동) — 메뉴에 닿는 값이 같으면 네이티브를 건드리지 않는다.
      key = JSON.stringify(snapshot);
      if (built !== null && key === appliedKey) return;
      // 같은 상태에서 이미 실패했다 — 실패 알림이 스토어를 바꿔 다시 불러도 되풀이하지 않는다.
      if (key === failedKey) return;
      const spec = buildNativeMenu(snapshot);
      if (built === null) built = await buildMenuBar(spec);
      else await syncChildren(null, built.roots, spec);
      // 반영하는 사이 항목을 골랐다면(체크 되돌리기) 이 반영을 최신으로 치지 않는다.
      appliedKey = generation === startGeneration ? key : null;
      failedKey = null;
      failing = false;
    } catch {
      appliedKey = null;
      failedKey = key;
      if (!failing) {
        failing = true;
        current.onError();
      }
    }
  }

  return {
    install(next) {
      options = next;
      // 새 설치(컨트롤러 교체·다시 마운트)는 실패했던 상태를 다시 시도할 기회다.
      failedKey = null;
      failing = false;
      const stopKeys = (next.keydowns ?? windowKeydowns)((event) => echo.record(event));
      const stopSettles = (next.settles ?? windowSettles)(schedule);
      const unsubscribes = [
        useWorkbenchStore.subscribe(schedule),
        usePreferences.subscribe(schedule),
        useI18nStore.subscribe(schedule),
        useShellProfileStore.subscribe(schedule),
        useNotificationStore.subscribe(schedule),
      ];
      if (shellsPlatform !== next.platform) {
        shellsPlatform = next.platform;
        void (next.detectShells ?? detectShellProfiles)(next.platform)
          .then((profiles) => {
            if (shellsPlatform !== next.platform) return;
            builtinShells = profiles;
            schedule();
          })
          .catch(() => undefined);
      }
      schedule();
      return () => {
        stopKeys();
        stopSettles();
        for (const unsubscribe of unsubscribes) unsubscribe();
        if (options === next) options = null;
      };
    },
    async flush() {
      for (;;) {
        const current = pending;
        await current;
        await Promise.resolve();
        if (current === pending && !scheduled) return;
      }
    },
  };
}

const appMenu = createNativeMenu(tauriMenuApi);

/**
 * 앱 메뉴(macOS 메뉴 막대, Windows 메뉴 모두)를 앱 기능 전체로 채우고 상태을 따라가게 한다.
 * 돌려준 함수는 구독만 푼다 — 메뉴는 남겨 두어 다음 설치(StrictMode·컨트롤러 교체)가 그대로 이어 쓴다.
 */
export function installNativeMenu(options: NativeMenuOptions): () => void {
  return appMenu.install(options);
}
