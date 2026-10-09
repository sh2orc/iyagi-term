/**
 * 끌어 놓기 재배치 모델(04-ui §2-5) — 순수 함수.
 *
 * 무엇을 끌고(탭·pane 헤더) 포인터가 어디에 있는지(탭 바의 틈·탭 위·pane의
 * 가장자리/가운데)에서 "놓으면 무슨 일이 일어나는가"를 정한다. DOM에서 좌표를
 * 읽고 미리보기를 그리는 일은 app/layoutDragSession·LayoutDragLayer가, 실행은
 * controller.applyLayoutDrop이 맡는다. 고르는 동작은 메뉴·팔레트에 이미 있는
 * 재배치(또는 그 확장)라 상한(탭당 8)·빈 탭 닫기 규칙이 같다.
 *
 * 규칙: 탭 "위"는 그 탭 안으로(pane 옮기기·탭 합치기), 탭 "사이"는 탭 자리로
 * (순서 바꾸기·새 탭으로 분리), pane 가장자리는 그 옆으로, pane 가운데는 자리
 * 바꾸기다.
 */

import { t, type MessageParams } from "../../i18n";
import type { TabState } from "../../store/workbenchStore";
import { terminalDisplayTitle } from "./shellEnvironment";
import { dockLeaf, findLeaf, leafCount, MAX_PANES_PER_TAB, type PaneEdge } from "./splitTree";

export interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * 끄는 것: 탭(탭 바·배치 편집의 그룹 카드), pane(헤더·블록), 또는 배치 안 된 실행 중
 * 세션(배치 편집 오른쪽 목록 — 어느 pane에도 붙어 있지 않은 터미널).
 */
export type LayoutDragSource =
  | { kind: "tab"; tabId: string }
  | { kind: "pane"; leafId: string }
  | { kind: "session"; sessionId: string; workloadId: string | null };

/** pane 위의 놓을 자리: 가장자리 넷(그 옆에 놓기)과 가운데(자리 바꾸기). */
export type PaneZone = PaneEdge | "center";

export type LayoutDropTarget =
  /** 탭 바의 틈 — index는 지금 탭 배열에 끼워 넣을 자리(0…탭 수). */
  | { kind: "tab-gap"; index: number }
  /** 탭 위. */
  | { kind: "tab"; tabId: string }
  /** 화면에 보이는 pane 위. */
  | { kind: "pane"; leafId: string; zone: PaneZone };

export type LayoutDropAction =
  /** focusLeafId: 혼자인 창의 헤더로 끌어 온 경우 — 놓은 뒤 그 탭을 보이고 그 창에 초점을 준다. */
  | { kind: "reorder-tab"; tabId: string; toIndex: number; focusLeafId?: string }
  | { kind: "merge-tab"; sourceTabId: string; targetTabId: string }
  | { kind: "move-pane-to-tab"; leafId: string; targetTabId: string }
  | { kind: "detach-pane"; leafId: string; atIndex: number }
  | { kind: "dock-pane"; leafId: string; targetLeafId: string; edge: PaneEdge }
  | { kind: "swap-panes"; leafId: string; targetLeafId: string }
  /** 배치 안 된(창만 닫기·그룹 삭제로 떨어져 나간) 실행 중 터미널을 그 자리에 새로 붙인다. */
  | { kind: "attach-session"; sessionId: string; workloadId: string | null; placement: LayoutPlacement };

/** 새로 붙일 터미널의 자리: 그 탭 안(초점 pane 옆) · 어느 pane의 가장자리 옆 · 틈의 새 탭. */
export type LayoutPlacement =
  | { kind: "tab"; tabId: string }
  | { kind: "beside"; leafId: string; edge: PaneEdge }
  | { kind: "new-tab"; atIndex: number };

/** 놓을 수 없는 이유 — 대상 탭 고르기 대화상자(MoveTargetDialog)와 같은 문구다. */
export type LayoutDropBlock =
  | { key: "moveTarget.full"; max: number }
  | { key: "moveTarget.tooMany"; n: number; max: number };

export interface LayoutDropResolution {
  /** 놓으면 할 동작. null이면 아무 일도 없다(대상 없음·제자리·막힘). */
  action: LayoutDropAction | null;
  /** 막힌 이유 — 있으면 action은 null이고 미리보기는 막힘으로 그린다. */
  blocked: LayoutDropBlock | null;
}

type TerminalTab = Extract<TabState, { kind: "terminal" }>;

const NOTHING: LayoutDropResolution = { action: null, blocked: null };
const act = (action: LayoutDropAction): LayoutDropResolution => ({ action, blocked: null });
const block = (blocked: LayoutDropBlock): LayoutDropResolution => ({ action: null, blocked });
/** "이미 그 자리인가"를 미리 계산할 때만 쓰는 split id — 실제 트리에는 들어가지 않는다. */
const PREVIEW_SPLIT_ID = "layout-drag-preview";

/** pane 가운데(자리 바꾸기) 상자가 차지하는 비율 — 가로·세로 각각. */
export const PANE_CENTER_FRACTION = 0.4;

/**
 * pane 안의 포인터 위치 → 놓을 자리. 가운데 상자 안이면 자리 바꾸기, 밖이면
 * 정규화한 거리로 가장 가까운 가장자리다 — 가로로 긴 pane에서도 대각선이 경계가
 * 되어 네 방향 영역이 고르게 나뉜다.
 */
export function paneZoneAt(rect: Rect, x: number, y: number): PaneZone {
  const rx = rect.width > 0 ? (x - rect.left) / rect.width : 0.5;
  const ry = rect.height > 0 ? (y - rect.top) / rect.height : 0.5;
  const margin = (1 - PANE_CENTER_FRACTION) / 2;
  if (rx >= margin && rx <= 1 - margin && ry >= margin && ry <= 1 - margin) return "center";
  const edges: Array<[PaneEdge, number]> = [
    ["left", rx],
    ["right", 1 - rx],
    ["top", ry],
    ["bottom", 1 - ry],
  ];
  let nearest = edges[0];
  for (const edge of edges) if (edge[1] < nearest[1]) nearest = edge;
  return nearest[0];
}

/** 놓았을 때 옮긴 창이 차지할 자리(미리보기) — 가장자리면 그 반쪽, 가운데면 전체. */
export function zonePreviewRect(rect: Rect, zone: PaneZone): Rect {
  const halfWidth = rect.width / 2;
  const halfHeight = rect.height / 2;
  switch (zone) {
    case "left":
      return { left: rect.left, top: rect.top, width: halfWidth, height: rect.height };
    case "right":
      return { left: rect.left + halfWidth, top: rect.top, width: halfWidth, height: rect.height };
    case "top":
      return { left: rect.left, top: rect.top, width: rect.width, height: halfHeight };
    case "bottom":
      return { left: rect.left, top: rect.top + halfHeight, width: rect.width, height: halfHeight };
    case "center":
      return { left: rect.left, top: rect.top, width: rect.width, height: rect.height };
  }
}

/**
 * 배치 안 된 세션을 pane 위에 놓을 자리 — 가운데가 없다(자리를 바꿀 상대가 없다). 가운데
 * 상자에서는 그 pane의 긴 변 쪽(가로로 길면 오른쪽, 세로로 길면 아래)에 붙인다.
 */
export function attachZoneAt(rect: Rect, x: number, y: number): PaneEdge {
  const zone = paneZoneAt(rect, x, y);
  if (zone !== "center") return zone;
  return rect.width >= rect.height ? "right" : "bottom";
}

/** 탭 바에 그려진 탭 하나의 가로 범위(화면 좌표). 배열은 탭 순서대로다. */
export interface TabSlot {
  tabId: string;
  left: number;
  right: number;
}

/** 탭 양끝에서 "틈"으로 보는 폭: 탭 폭의 1/4을 8~32px로 자른다. */
export function tabEdgePx(width: number): number {
  return Math.min(32, Math.max(8, width / 4));
}

/**
 * 탭 바 위 포인터 x → 틈 또는 탭 위. 위에 놓을 수 없는 탭(canDropOnto가 false —
 * mission 계열 탭, 끌고 있는 탭 자신)은 반으로 나눠 틈만 고른다.
 */
export function tabBarTargetAt(
  slots: readonly TabSlot[],
  x: number,
  canDropOnto: (tabId: string) => boolean,
): Extract<LayoutDropTarget, { kind: "tab-gap" | "tab" }> {
  for (let index = 0; index < slots.length; index += 1) {
    const slot = slots[index];
    if (x < slot.left) return { kind: "tab-gap", index };
    if (x > slot.right) continue;
    const width = slot.right - slot.left;
    if (!canDropOnto(slot.tabId)) {
      return { kind: "tab-gap", index: x < slot.left + width / 2 ? index : index + 1 };
    }
    const edge = tabEdgePx(width);
    if (x < slot.left + edge) return { kind: "tab-gap", index };
    if (x > slot.right - edge) return { kind: "tab-gap", index: index + 1 };
    return { kind: "tab", tabId: slot.tabId };
  }
  return { kind: "tab-gap", index: slots.length };
}

/** 틈만 고른다(탭 위를 반으로 나눔) — 탭을 끌며 지나가는 동안은 순서 바꾸기다. */
export function tabGapAt(slots: readonly TabSlot[], x: number): Extract<LayoutDropTarget, { kind: "tab-gap" }> {
  const target = tabBarTargetAt(slots, x, () => false);
  return target.kind === "tab-gap" ? target : { kind: "tab-gap", index: slots.length };
}

/** 틈 index에 그릴 삽입 막대의 x(화면 좌표). 탭이 없으면 null. */
export function tabGapX(slots: readonly TabSlot[], index: number): number | null {
  if (slots.length === 0) return null;
  if (index <= 0) return slots[0].left;
  if (index >= slots.length) return slots[slots.length - 1].right;
  return (slots[index - 1].right + slots[index].left) / 2;
}

/** 이 탭 "위"에 놓는 뜻이 있는가: pane은 terminal 탭 안으로, 탭은 다른 terminal 탭에 합치기. */
export function canDropOntoTab(tabs: readonly TabState[], source: LayoutDragSource, tabId: string): boolean {
  const target = tabs.find((tab) => tab.id === tabId);
  if (!target || target.kind !== "terminal") return false;
  // pane·배치 안 된 세션은 terminal 탭 안으로 들어간다.
  if (source.kind !== "tab") return true;
  const sourceTab = tabs.find((tab) => tab.id === source.tabId);
  return sourceTab?.kind === "terminal" && sourceTab.id !== target.id;
}

/**
 * 끄는 것·놓을 자리 → 놓으면 할 동작. 제자리(바뀌는 것이 없음)·대상 없음은 아무
 * 동작도 없고, 상한에 걸리면 이유를 돌려준다(놓아도 바꾸지 않는다).
 */
export function resolveLayoutDrop(
  tabs: readonly TabState[],
  source: LayoutDragSource,
  target: LayoutDropTarget | null,
  maxPerTab: number = MAX_PANES_PER_TAB,
): LayoutDropResolution {
  if (!target) return NOTHING;
  if (source.kind === "tab") return resolveTabDrop(tabs, source.tabId, target, maxPerTab);
  if (source.kind === "session") return resolveSessionDrop(tabs, source, target, maxPerTab);

  const sourceTab = tabHoldingLeaf(tabs, source.leafId);
  if (!sourceTab?.root) return NOTHING;

  if (target.kind === "tab-gap") {
    if (leafCount(sourceTab.root) <= 1) {
      // 혼자인 pane은 이미 탭 하나다 — 틈에 놓으면 그 탭을 그 자리로 옮기고, 다른 창 동작처럼
      // 초점이 그 창을 따라간다(머물러 다른 탭을 열어 둔 채 놓아도 그 탭이 다시 보인다).
      const moved = resolveTabDrop(tabs, sourceTab.id, target, maxPerTab);
      return moved.action?.kind === "reorder-tab" ? act({ ...moved.action, focusLeafId: source.leafId }) : moved;
    }
    return act({ kind: "detach-pane", leafId: source.leafId, atIndex: target.index });
  }

  if (target.kind === "tab") {
    const targetTab = tabs.find((tab) => tab.id === target.tabId);
    if (!targetTab || targetTab.kind !== "terminal" || targetTab.id === sourceTab.id) return NOTHING;
    if (leafCount(targetTab.root) >= maxPerTab) return block({ key: "moveTarget.full", max: maxPerTab });
    return act({ kind: "move-pane-to-tab", leafId: source.leafId, targetTabId: targetTab.id });
  }

  if (target.leafId === source.leafId) return NOTHING;
  const targetTab = tabHoldingLeaf(tabs, target.leafId);
  if (!targetTab?.root) return NOTHING;
  // 자리 바꾸기는 창 수가 그대로라 상한과 무관하다.
  if (target.zone === "center") {
    return act({ kind: "swap-panes", leafId: source.leafId, targetLeafId: target.leafId });
  }
  if (targetTab.id === sourceTab.id) {
    // 같은 탭: 결과가 지금과 같은 모양(이미 그 자리)이면 놓아도 바뀌지 않는다.
    const next = dockLeaf(sourceTab.root, source.leafId, target.leafId, target.zone, PREVIEW_SPLIT_ID);
    if (!next || next === sourceTab.root) return NOTHING;
  } else if (leafCount(targetTab.root) >= maxPerTab) {
    return block({ key: "moveTarget.full", max: maxPerTab });
  }
  return act({ kind: "dock-pane", leafId: source.leafId, targetLeafId: target.leafId, edge: target.zone });
}

function resolveTabDrop(
  tabs: readonly TabState[],
  tabId: string,
  target: LayoutDropTarget,
  maxPerTab: number,
): LayoutDropResolution {
  const from = tabs.findIndex((tab) => tab.id === tabId);
  if (from < 0) return NOTHING;
  if (target.kind === "tab-gap") {
    // 틈 index는 옮기기 전 배열 기준이다 — 자기보다 뒤의 틈이면 빠진 자리만큼 당긴다.
    const toIndex = target.index > from ? target.index - 1 : target.index;
    return toIndex === from ? NOTHING : act({ kind: "reorder-tab", tabId, toIndex });
  }
  // 탭을 pane 위에 놓는 동작은 없다.
  if (target.kind !== "tab") return NOTHING;
  const source = tabs[from];
  const targetTab = tabs.find((tab) => tab.id === target.tabId);
  if (!targetTab || targetTab.id === source.id || source.kind !== "terminal" || targetTab.kind !== "terminal") {
    return NOTHING;
  }
  const n = leafCount(source.root) + leafCount(targetTab.root);
  if (n > maxPerTab) return block({ key: "moveTarget.tooMany", n, max: maxPerTab });
  return act({ kind: "merge-tab", sourceTabId: source.id, targetTabId: targetTab.id });
}

/**
 * 배치 안 된 세션: 틈이면 새 탭, 탭 위면 그 탭 안, pane 위면 그 가장자리 옆에 새로 붙인다.
 * 창이 하나 늘어나므로 대상 탭의 상한(8)을 본다 — pane 이동과 같은 문구로 막는다.
 */
function resolveSessionDrop(
  tabs: readonly TabState[],
  source: Extract<LayoutDragSource, { kind: "session" }>,
  target: LayoutDropTarget,
  maxPerTab: number,
): LayoutDropResolution {
  const attach = (placement: LayoutPlacement) =>
    act({ kind: "attach-session", sessionId: source.sessionId, workloadId: source.workloadId, placement });
  if (target.kind === "tab-gap") return attach({ kind: "new-tab", atIndex: target.index });
  const targetTab =
    target.kind === "tab" ? tabs.find((tab) => tab.id === target.tabId) : tabHoldingLeaf(tabs, target.leafId);
  if (!targetTab || targetTab.kind !== "terminal") return NOTHING;
  if (leafCount(targetTab.root) >= maxPerTab) return block({ key: "moveTarget.full", max: maxPerTab });
  if (target.kind === "tab") return attach({ kind: "tab", tabId: targetTab.id });
  // 가운데에는 자리를 바꿀 상대가 없다 — 판정 쪽(attachZoneAt)이 긴 변으로 바꿔 주고, 모델은 오른쪽으로 받는다.
  return attach({ kind: "beside", leafId: target.leafId, edge: target.zone === "center" ? "right" : target.zone });
}

/** leaf가 들어 있는 terminal 탭(없으면 null). */
function tabHoldingLeaf(tabs: readonly TabState[], leafId: string): TerminalTab | null {
  for (const tab of tabs) {
    if (tab.kind === "terminal" && findLeaf(tab.root, leafId) !== null) return tab;
  }
  return null;
}

/** 안내 문구에 쓰는 탭 이름 — 탭 바에 보이는 이름과 같은 규칙(이름이 없으면 "탭 N"). */
export function layoutTabName(tabs: readonly TabState[], tabId: string): string {
  const index = tabs.findIndex((tab) => tab.id === tabId);
  const tab = tabs[index];
  if (!tab) return "";
  if (!tab.title) return t("app.tabTitle", { index: index + 1 });
  return tab.kind === "terminal" ? terminalDisplayTitle(tab.title) : tab.title;
}

/** 안내 문구의 단위: 탭 화면은 "탭", 배치 편집은 "그룹"(그룹 하나가 탭 하나). */
export type LayoutDragUnit = "tab" | "group";

/** 놓으면 무슨 일이 일어나는지(커서 옆 안내). 아무 일도 없으면 null. */
export function layoutDropText(
  tabs: readonly TabState[],
  resolution: LayoutDropResolution,
  unit: LayoutDragUnit = "tab",
): string | null {
  if (resolution.blocked) return layoutDropBlockText(resolution.blocked);
  const action = resolution.action;
  if (!action) return null;
  const phrase = (key: string, params?: MessageParams) =>
    t(unit === "group" ? `app.drag.group.${key}` : `app.drag.${key}`, params);
  switch (action.kind) {
    case "reorder-tab":
      return phrase("reorderTab");
    case "merge-tab":
      return phrase("mergeTab", { title: layoutTabName(tabs, action.targetTabId) });
    case "move-pane-to-tab":
      return phrase("movePane", { title: layoutTabName(tabs, action.targetTabId) });
    case "detach-pane":
      return phrase("detachPane");
    case "dock-pane":
      return t(`app.drag.dock.${action.edge}`);
    case "swap-panes":
      return t("app.drag.swap");
    case "attach-session":
      if (action.placement.kind === "beside") return t(`app.drag.dock.${action.placement.edge}`);
      if (action.placement.kind === "new-tab") return phrase("attachNewTab");
      return phrase("attachTab", { title: layoutTabName(tabs, action.placement.tabId) });
  }
}

/** 막힌 이유 문구(대상 탭 고르기 대화상자와 같은 키). */
export function layoutDropBlockText(reason: LayoutDropBlock): string {
  return reason.key === "moveTarget.full"
    ? t("moveTarget.full", { max: reason.max })
    : t("moveTarget.tooMany", { n: reason.n, max: reason.max });
}
