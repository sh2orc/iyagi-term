/**
 * pane 메뉴의 완화 항목(08-pressure-relief §2): 지금 할 수 있는 것만 보이고,
 * 되돌릴 수 없는 플랫폼에서는 흐리게 남되 사유가 툴팁으로 남는다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { t } from "../../i18n";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";
import { reliefMenuItems, type ReliefCapability } from "./reliefMenu";
import type { PaneMenuController } from "./paneMenuActions";

function fakeController(): PaneMenuController & { paneRelief: ReturnType<typeof vi.fn> } {
  return {
    copySelection: vi.fn(async () => true),
    pasteFromClipboard: vi.fn(async () => undefined),
    selectAllInPane: vi.fn(),
    dispatchShortcut: vi.fn(),
    clearPaneScrollback: vi.fn(),
    toggleBroadcast: vi.fn(),
    copyText: vi.fn(async () => true),
    retryPane: vi.fn(),
    requestClosePanes: vi.fn(),
    focusPaneTerminal: vi.fn(),
    detachPaneToNewTab: vi.fn(() => true),
    paneRelief: vi.fn(async () => undefined),
  } as unknown as PaneMenuController & { paneRelief: ReturnType<typeof vi.fn> };
}

function pane(overrides: Partial<PaneMeta> = {}): PaneMeta {
  return {
    leafId: "leaf-1",
    viewId: "view-1",
    sessionId: "s1",
    workloadId: "w1",
    title: "zsh",
    cwd: "/work",
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
    relief: { kind: "NONE" },
    protected: false,
    ...overrides,
  };
}

const supported: ReliefCapability = { support: "supported", reason: null };

function labels(p: PaneMeta, capability: ReliefCapability = supported): string[] {
  return reliefMenuItems(fakeController(), "leaf-1", p, capability, t).map((item) => item.label);
}

afterEach(() => useWorkbenchStore.setState({ panes: {}, focusedLeafId: null }));

describe("reliefMenuItems", () => {
  it("평소에는 양보와 보호를 보인다", () => {
    expect(labels(pane())).toEqual([t("app.paneMenu.reliefYield"), t("app.paneMenu.reliefProtect")]);
  });

  it("양보 중이면 양보 대신 해제를 보인다", () => {
    const yielded = pane({ relief: { kind: "YIELDED", sinceMs: 1, manual: false, partial: false } });
    expect(labels(yielded)).toEqual([
      t("app.paneMenu.reliefRestore"),
      t("app.paneMenu.reliefProtect"),
    ]);
  });

  it("보호 중이면 양보를 감추고 보호 해제를 보인다", () => {
    expect(labels(pane({ protected: true }))).toEqual([t("app.paneMenu.reliefUnprotect")]);
  });

  it("보호 중이라도 이미 양보된 세션은 해제할 수 있다", () => {
    const both = pane({
      relief: { kind: "YIELDED", sinceMs: 1, manual: true, partial: false },
      protected: true,
    });
    expect(labels(both)).toEqual([
      t("app.paneMenu.reliefRestore"),
      t("app.paneMenu.reliefUnprotect"),
    ]);
  });

  it("세션이 없는 pane에는 완화할 프로세스가 없어 항목을 넣지 않는다", () => {
    expect(labels(pane({ sessionId: null, phase: "exited" }))).toEqual([]);
  });

  it("지원하지 않는 플랫폼에서는 흐리게 두고 데몬의 사유를 툴팁에 싣는다", () => {
    const reason = "위임 cgroup이 없어 되돌릴 수 없습니다";
    const items = reliefMenuItems(
      fakeController(),
      "leaf-1",
      pane(),
      { support: "unsupported", reason },
      t,
    );
    expect(items.map((item) => item.disabled)).toEqual([true, true]);
    expect(items.map((item) => item.title)).toEqual([reason, reason]);
  });

  it("구 데몬(capability 미보고)도 지원하지 않는 것으로 보고 기본 사유를 쓴다", () => {
    const items = reliefMenuItems(fakeController(), "leaf-1", pane(), { support: null, reason: null }, t);
    expect(items.every((item) => item.disabled)).toBe(true);
    expect(items[0].title).toBe(t("terminal.relief.unsupported"));
  });

  it("고르면 그 pane의 완화 액션을 컨트롤러로 보낸다", () => {
    const controller = fakeController();
    useWorkbenchStore.setState({ panes: { "leaf-1": pane() } });
    const items = reliefMenuItems(controller, "leaf-1", pane(), supported, t);
    items[0].onSelect();
    items[1].onSelect();
    expect(controller.paneRelief.mock.calls).toEqual([
      ["leaf-1", "yield"],
      ["leaf-1", "protect"],
    ]);
  });
});
