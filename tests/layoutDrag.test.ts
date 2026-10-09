/**
 * 끌어 놓기 재배치 모델(04-ui §2-5): 포인터 자리 → 놓을 자리 → 놓으면 할 동작.
 * DOM 없이 좌표·탭 배열만으로 계약을 확인한다(세션 glue는 이 모델을 부를 뿐이다).
 */

import { describe, expect, it } from "vitest";
import {
  attachZoneAt,
  canDropOntoTab,
  layoutDropText,
  layoutTabName,
  paneZoneAt,
  resolveLayoutDrop,
  tabBarTargetAt,
  tabEdgePx,
  tabGapAt,
  tabGapX,
  zonePreviewRect,
  type TabSlot,
} from "../src/features/terminal/layoutDrag";
import { makeLeaf, type SplitNode } from "../src/features/terminal/splitTree";
import type { TabState } from "../src/store/workbenchStore";
import { t } from "../src/i18n";

const leaf = (id: string) => makeLeaf(id, `v-${id}`, `s-${id}`);

function chainRow(ids: string[]): SplitNode {
  let acc: SplitNode = leaf(ids[0]);
  for (let i = 1; i < ids.length; i += 1) {
    acc = { kind: "split", id: `r-${ids.slice(0, i + 1).join("")}`, axis: "row", ratio: 0.5, first: acc, second: leaf(ids[i]) };
  }
  return acc;
}

/** t1 = a|b, t2 = c, t3 = mission, t4 = 창 8개(가득), t5 = 이름 없는 빈 terminal 탭. */
function world(): TabState[] {
  return [
    { kind: "terminal", id: "t1", title: "api", root: chainRow(["a", "b"]) },
    { kind: "terminal", id: "t2", title: "web", root: leaf("c") },
    { kind: "mission", id: "t3", title: "미션", missionId: "m1" },
    { kind: "terminal", id: "t4", title: "full", root: chainRow(["d0", "d1", "d2", "d3", "d4", "d5", "d6", "d7"]) },
    { kind: "terminal", id: "t5", title: "", root: null },
  ];
}

const NOTHING = { action: null, blocked: null };

describe("paneZoneAt / zonePreviewRect", () => {
  const rect = { left: 100, top: 50, width: 400, height: 200 };

  it("picks the center box, otherwise the nearest edge by normalised distance", () => {
    expect(paneZoneAt(rect, 300, 150)).toBe("center");
    expect(paneZoneAt(rect, 110, 150)).toBe("left");
    expect(paneZoneAt(rect, 490, 150)).toBe("right");
    expect(paneZoneAt(rect, 300, 55)).toBe("top");
    expect(paneZoneAt(rect, 300, 245)).toBe("bottom");
    expect(paneZoneAt(rect, 115, 62)).toBe("left");
    expect(paneZoneAt(rect, 140, 55)).toBe("top");
    expect(paneZoneAt({ left: 0, top: 0, width: 0, height: 0 }, 5, 5)).toBe("center");
  });

  it("previews the half the moved pane will take, or the whole pane for a swap", () => {
    expect(zonePreviewRect(rect, "left")).toEqual({ left: 100, top: 50, width: 200, height: 200 });
    expect(zonePreviewRect(rect, "right")).toEqual({ left: 300, top: 50, width: 200, height: 200 });
    expect(zonePreviewRect(rect, "top")).toEqual({ left: 100, top: 50, width: 400, height: 100 });
    expect(zonePreviewRect(rect, "bottom")).toEqual({ left: 100, top: 150, width: 400, height: 100 });
    expect(zonePreviewRect(rect, "center")).toEqual(rect);
  });
});

describe("tabBarTargetAt / tabGapAt / tabGapX", () => {
  const slots: TabSlot[] = [
    { tabId: "t1", left: 0, right: 100 },
    { tabId: "t2", left: 102, right: 202 },
    { tabId: "t3", left: 204, right: 304 },
  ];
  const any = () => true;

  it("splits a droppable tab into edge gaps and a middle 'onto' zone", () => {
    expect(tabEdgePx(100)).toBe(25);
    expect(tabEdgePx(10)).toBe(8);
    expect(tabEdgePx(400)).toBe(32);
    expect(tabBarTargetAt(slots, -5, any)).toEqual({ kind: "tab-gap", index: 0 });
    expect(tabBarTargetAt(slots, 101, any)).toEqual({ kind: "tab-gap", index: 1 });
    expect(tabBarTargetAt(slots, 110, any)).toEqual({ kind: "tab-gap", index: 1 });
    expect(tabBarTargetAt(slots, 150, any)).toEqual({ kind: "tab", tabId: "t2" });
    expect(tabBarTargetAt(slots, 195, any)).toEqual({ kind: "tab-gap", index: 2 });
    // 마지막 탭 뒤(+ 버튼 쪽 빈 곳)는 맨 끝 틈이다.
    expect(tabBarTargetAt(slots, 400, any)).toEqual({ kind: "tab-gap", index: 3 });
  });

  it("halves a tab that cannot take a drop, and tabGapAt always picks a gap", () => {
    const notT2 = (id: string) => id !== "t2";
    expect(tabBarTargetAt(slots, 140, notT2)).toEqual({ kind: "tab-gap", index: 1 });
    expect(tabBarTargetAt(slots, 160, notT2)).toEqual({ kind: "tab-gap", index: 2 });
    expect(tabGapAt(slots, 150)).toEqual({ kind: "tab-gap", index: 1 });
    expect(tabGapAt(slots, 250)).toEqual({ kind: "tab-gap", index: 2 });
  });

  it("places the insert marker between tabs and at both ends", () => {
    expect(tabGapX(slots, 0)).toBe(0);
    expect(tabGapX(slots, 1)).toBe(101);
    expect(tabGapX(slots, 3)).toBe(304);
    expect(tabGapX([], 0)).toBeNull();
  });
});

describe("canDropOntoTab", () => {
  const tabs = world();

  it("lets panes into terminal tabs, and tabs onto other terminal tabs only", () => {
    expect(canDropOntoTab(tabs, { kind: "pane", leafId: "a" }, "t2")).toBe(true);
    expect(canDropOntoTab(tabs, { kind: "pane", leafId: "a" }, "t3")).toBe(false);
    expect(canDropOntoTab(tabs, { kind: "tab", tabId: "t1" }, "t2")).toBe(true);
    expect(canDropOntoTab(tabs, { kind: "tab", tabId: "t1" }, "t1")).toBe(false);
    expect(canDropOntoTab(tabs, { kind: "tab", tabId: "t3" }, "t2")).toBe(false);
    expect(canDropOntoTab(tabs, { kind: "tab", tabId: "t1" }, "nope")).toBe(false);
  });
});

describe("resolveLayoutDrop — 탭을 끌 때", () => {
  const tabs = world();
  const tab = (tabId: string) => ({ kind: "tab" as const, tabId });

  it("reorders into a gap, counting the slot the tab leaves, and ignores gaps that change nothing", () => {
    expect(resolveLayoutDrop(tabs, tab("t1"), { kind: "tab-gap", index: 3 }).action).toEqual({
      kind: "reorder-tab", tabId: "t1", toIndex: 2,
    });
    expect(resolveLayoutDrop(tabs, tab("t4"), { kind: "tab-gap", index: 0 }).action).toEqual({
      kind: "reorder-tab", tabId: "t4", toIndex: 0,
    });
    expect(resolveLayoutDrop(tabs, tab("t2"), { kind: "tab-gap", index: 1 })).toEqual(NOTHING);
    expect(resolveLayoutDrop(tabs, tab("t2"), { kind: "tab-gap", index: 2 })).toEqual(NOTHING);
    // 순서 이동은 탭 종류와 무관하다.
    expect(resolveLayoutDrop(tabs, tab("t3"), { kind: "tab-gap", index: 0 }).action).toEqual({
      kind: "reorder-tab", tabId: "t3", toIndex: 0,
    });
  });

  it("merges onto another terminal tab within the cap and explains an over-cap merge", () => {
    expect(resolveLayoutDrop(tabs, tab("t2"), tab("t1")).action).toEqual({
      kind: "merge-tab", sourceTabId: "t2", targetTabId: "t1",
    });
    expect(resolveLayoutDrop(tabs, tab("t1"), tab("t4"))).toEqual({
      action: null, blocked: { key: "moveTarget.tooMany", n: 10, max: 8 },
    });
    // 빈 탭 합치기는 그 탭 닫기와 같다(store.mergeTabs) — 막지 않는다.
    expect(resolveLayoutDrop(tabs, tab("t5"), tab("t2")).action).toEqual({
      kind: "merge-tab", sourceTabId: "t5", targetTabId: "t2",
    });
    expect(resolveLayoutDrop(tabs, tab("t1"), tab("t1"))).toEqual(NOTHING);
    expect(resolveLayoutDrop(tabs, tab("t3"), tab("t1"))).toEqual(NOTHING);
    expect(resolveLayoutDrop(tabs, tab("t1"), { kind: "pane", leafId: "c", zone: "left" })).toEqual(NOTHING);
    expect(resolveLayoutDrop(tabs, tab("t1"), null)).toEqual(NOTHING);
  });
});

describe("resolveLayoutDrop — pane 헤더를 끌 때", () => {
  const tabs = world();
  const pane = (leafId: string) => ({ kind: "pane" as const, leafId });

  it("opens a new tab at the gap, or moves the whole tab when the pane is alone in it", () => {
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "tab-gap", index: 5 }).action).toEqual({
      kind: "detach-pane", leafId: "a", atIndex: 5,
    });
    // 초점은 끌어 온 창을 따라간다 — 머물러 다른 탭을 열었어도 그 탭이 다시 보인다.
    expect(resolveLayoutDrop(tabs, pane("c"), { kind: "tab-gap", index: 0 }).action).toEqual({
      kind: "reorder-tab", tabId: "t2", toIndex: 0, focusLeafId: "c",
    });
    expect(resolveLayoutDrop(tabs, pane("c"), { kind: "tab-gap", index: 2 })).toEqual(NOTHING);
  });

  it("moves onto another terminal tab and states why a full tab refuses", () => {
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "tab", tabId: "t2" }).action).toEqual({
      kind: "move-pane-to-tab", leafId: "a", targetTabId: "t2",
    });
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "tab", tabId: "t5" }).action).toEqual({
      kind: "move-pane-to-tab", leafId: "a", targetTabId: "t5",
    });
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "tab", tabId: "t4" })).toEqual({
      action: null, blocked: { key: "moveTarget.full", max: 8 },
    });
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "tab", tabId: "t1" })).toEqual(NOTHING);
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "tab", tabId: "t3" })).toEqual(NOTHING);
  });

  it("docks beside a pane, swaps on its center, and ignores drops that leave the layout as it is", () => {
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "pane", leafId: "b", zone: "right" }).action).toEqual({
      kind: "dock-pane", leafId: "a", targetLeafId: "b", edge: "right",
    });
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "pane", leafId: "b", zone: "left" })).toEqual(NOTHING);
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "pane", leafId: "b", zone: "bottom" }).action).toEqual({
      kind: "dock-pane", leafId: "a", targetLeafId: "b", edge: "bottom",
    });
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "pane", leafId: "b", zone: "center" }).action).toEqual({
      kind: "swap-panes", leafId: "a", targetLeafId: "b",
    });
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "pane", leafId: "a", zone: "left" })).toEqual(NOTHING);
  });

  it("docks into another tab within the cap, while a swap ignores the cap", () => {
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "pane", leafId: "c", zone: "top" }).action).toEqual({
      kind: "dock-pane", leafId: "a", targetLeafId: "c", edge: "top",
    });
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "pane", leafId: "d3", zone: "left" })).toEqual({
      action: null, blocked: { key: "moveTarget.full", max: 8 },
    });
    expect(resolveLayoutDrop(tabs, pane("a"), { kind: "pane", leafId: "d3", zone: "center" }).action).toEqual({
      kind: "swap-panes", leafId: "a", targetLeafId: "d3",
    });
  });
});

describe("layoutDropText / layoutTabName", () => {
  const tabs = world();

  it("names the target tab and edge, reuses the dialog's cap text, and says nothing for no-ops", () => {
    expect(layoutDropText(tabs, { action: { kind: "merge-tab", sourceTabId: "t2", targetTabId: "t1" }, blocked: null })).toBe(
      t("app.drag.mergeTab", { title: "api" }),
    );
    expect(layoutDropText(tabs, { action: { kind: "move-pane-to-tab", leafId: "a", targetTabId: "t2" }, blocked: null })).toBe(
      t("app.drag.movePane", { title: "web" }),
    );
    expect(
      layoutDropText(tabs, { action: { kind: "dock-pane", leafId: "a", targetLeafId: "c", edge: "left" }, blocked: null }),
    ).toBe(t("app.drag.dock.left"));
    expect(layoutDropText(tabs, { action: null, blocked: { key: "moveTarget.full", max: 8 } })).toBe(
      t("moveTarget.full", { max: 8 }),
    );
    expect(layoutDropText(tabs, NOTHING)).toBeNull();
    expect(layoutTabName(tabs, "t5")).toBe(t("app.tabTitle", { index: 5 }));
    expect(layoutTabName(tabs, "nope")).toBe("");
  });

  it("has a dictionary entry for every drag phrase (no raw keys leak)", () => {
    const keys = [
      "app.drag.paneHint", "app.drag.idle.pane", "app.drag.idle.tab", "app.drag.reorderTab", "app.drag.mergeTab",
      "app.drag.movePane", "app.drag.detachPane", "app.drag.dock.left", "app.drag.dock.right", "app.drag.dock.top",
      "app.drag.dock.bottom", "app.drag.swap", "app.drag.dwellMerge", "app.drag.dwellOpen",
    ];
    for (const key of keys) expect(t(key), key).not.toBe(key);
  });
});

describe("resolveLayoutDrop — 배치 안 된 세션을 끌 때(배치 편집, 04-ui §2-6)", () => {
  const tabs = world();
  const session = { kind: "session" as const, sessionId: "s-u", workloadId: "w-u" };
  const attach = (placement: object) => ({ kind: "attach-session", sessionId: "s-u", workloadId: "w-u", placement });

  it("opens a new tab at a gap and joins a terminal tab, refusing a full tab and a mission tab", () => {
    expect(resolveLayoutDrop(tabs, session, { kind: "tab-gap", index: 5 }).action).toEqual(attach({ kind: "new-tab", atIndex: 5 }));
    expect(resolveLayoutDrop(tabs, session, { kind: "tab", tabId: "t2" }).action).toEqual(attach({ kind: "tab", tabId: "t2" }));
    expect(resolveLayoutDrop(tabs, session, { kind: "tab", tabId: "t5" }).action).toEqual(attach({ kind: "tab", tabId: "t5" }));
    expect(resolveLayoutDrop(tabs, session, { kind: "tab", tabId: "t4" })).toEqual({
      action: null, blocked: { key: "moveTarget.full", max: 8 },
    });
    expect(resolveLayoutDrop(tabs, session, { kind: "tab", tabId: "t3" })).toEqual(NOTHING);
    expect(canDropOntoTab(tabs, session, "t2")).toBe(true);
    expect(canDropOntoTab(tabs, session, "t3")).toBe(false);
  });

  it("attaches beside a pane edge, reads the center as the right edge, and respects the cap", () => {
    expect(resolveLayoutDrop(tabs, session, { kind: "pane", leafId: "b", zone: "bottom" }).action).toEqual(
      attach({ kind: "beside", leafId: "b", edge: "bottom" }),
    );
    expect(resolveLayoutDrop(tabs, session, { kind: "pane", leafId: "b", zone: "center" }).action).toEqual(
      attach({ kind: "beside", leafId: "b", edge: "right" }),
    );
    expect(resolveLayoutDrop(tabs, session, { kind: "pane", leafId: "d0", zone: "left" })).toEqual({
      action: null, blocked: { key: "moveTarget.full", max: 8 },
    });
  });

  it("attachZoneAt never answers center — the pane's long side wins", () => {
    expect(attachZoneAt({ left: 0, top: 0, width: 400, height: 100 }, 200, 50)).toBe("right");
    expect(attachZoneAt({ left: 0, top: 0, width: 100, height: 400 }, 50, 200)).toBe("bottom");
    expect(attachZoneAt({ left: 0, top: 0, width: 400, height: 100 }, 5, 50)).toBe("left");
  });
});

describe("layoutDropText — 배치 편집은 그룹이라 부른다", () => {
  const tabs = world();
  const attachTo = (placement: object) => ({
    action: { kind: "attach-session", sessionId: "s", workloadId: null, placement } as never,
    blocked: null,
  });

  it("switches the unit word and names where a detached terminal will go", () => {
    expect(
      layoutDropText(tabs, { action: { kind: "move-pane-to-tab", leafId: "a", targetTabId: "t2" }, blocked: null }, "group"),
    ).toBe(t("app.drag.group.movePane", { title: "web" }));
    expect(layoutDropText(tabs, { action: { kind: "reorder-tab", tabId: "t1", toIndex: 2 }, blocked: null }, "group")).toBe(
      t("app.drag.group.reorderTab"),
    );
    expect(layoutDropText(tabs, attachTo({ kind: "tab", tabId: "t1" }))).toBe(t("app.drag.attachTab", { title: "api" }));
    expect(layoutDropText(tabs, attachTo({ kind: "new-tab", atIndex: 0 }), "group")).toBe(t("app.drag.group.attachNewTab"));
    expect(layoutDropText(tabs, attachTo({ kind: "beside", leafId: "a", edge: "top" }), "group")).toBe(t("app.drag.dock.top"));
    for (const key of [
      "app.drag.attachTab", "app.drag.attachNewTab", "app.drag.group.reorderTab", "app.drag.group.mergeTab",
      "app.drag.group.movePane", "app.drag.group.detachPane", "app.drag.group.attachTab", "app.drag.group.attachNewTab",
      "app.drag.group.idle.pane", "app.drag.group.idle.tab", "app.drag.group.idle.session",
    ]) {
      expect(t(key), key).not.toBe(key);
    }
  });
});
