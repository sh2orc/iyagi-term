/**
 * 끌어 놓기 세션(04-ui §2-5): 탭이나 pane 헤더를 누른 채 움직이면 시작하고,
 * 포인터를 따라 놓을 자리를 고르다가, 놓으면 controller로 보낸다.
 *
 * 놓으면 무슨 일이 일어날지는 순수 모델(layoutDrag.ts)이 정한다. 여기서는 DOM에서
 * 좌표를 읽고(탭 바·pane의 화면 위치), 임계값·머무르기(탭 위에서 잠시 멈추면
 * 합치기가 켜지거나 그 탭이 열림)·자동 스크롤·취소(Esc·창 blur)·클릭 억제만
 * 맡는다. 미리보기는 useLayoutDragStore를 LayoutDragLayer가 그린다 — 끄는 동안
 * pane·탭 컴포넌트는 다시 그려지지 않는다.
 *
 * 리스너는 window에 건다: pane을 끌다 머물러 다른 탭이 열리면 누른 헤더가 화면에서
 * 빠진다. 요소에 건 pointer capture는 요소와 함께 풀리지만 window 리스너는 남는다.
 * capture도 함께 걸어 창 밖을 지나는 움직임을 놓치지 않게 한다.
 */

import type { PointerEvent as ReactPointerEvent } from "react";
import { create } from "zustand";
import { useWorkbenchStore, type WorkbenchState } from "../store/workbenchStore";
import type { SessionController } from "../features/terminal/sessionController";
import {
  attachZoneAt,
  canDropOntoTab,
  layoutDropBlockText,
  layoutTabName,
  paneZoneAt,
  resolveLayoutDrop,
  tabBarTargetAt,
  tabGapAt,
  tabGapX,
  zonePreviewRect,
  type LayoutDragSource,
  type LayoutDropResolution,
  type LayoutDropTarget,
  type PaneZone,
  type Rect,
  type TabSlot,
} from "../features/terminal/layoutDrag";
import { findLeaf } from "../features/terminal/splitTree";
import { terminalDisplayTitle } from "../features/terminal/shellEnvironment";
import { workloadListTitle } from "../features/workloads/workloadTitle";

/** 이만큼 움직여야 끌기다 — 그 전에 떼면 평소 클릭(탭 선택·pane 초점)이다. */
export const DRAG_THRESHOLD_PX = 5;
/**
 * 탭 위에서 이만큼 머무르면: 탭을 끌 때는 합치기가 켜지고(지나가다 실수로 합쳐지지
 * 않게), pane을 끌 때는 그 탭이 열린다(그 안의 원하는 자리에 놓을 수 있게).
 */
export const TAB_DWELL_MS = 550;
/** 넘친 탭 바의 양끝 이 폭 안에 머무르면 그쪽으로 저절로 스크롤한다. */
const AUTOSCROLL_EDGE_PX = 28;
const AUTOSCROLL_STEP_PX = 12;
/** 배치 편집 보드의 카드 사이 간격(layoutEditor.css의 gap) — 순서 삽입 막대를 그 가운데에 그린다. */
const CARD_GAP_PX = 16;
/** 이 위에서 누른 것은 끌기가 아니다(닫기·메뉴 버튼, 이름 편집 칸). */
const INTERACTIVE_SELECTOR = "button, input, textarea, select, a[href], [contenteditable='true']";

export type LayoutDragPreview =
  | { kind: "zone"; leafId: string; zone: PaneZone; rect: Rect }
  | { kind: "insert"; x: number; top: number; height: number }
  | { kind: "tab"; tabId: string; rect: Rect };

export interface LayoutDragDwell {
  tabId: string;
  /** 그 탭의 이름(안내 문구용). */
  title: string;
  rect: Rect;
  /** merge: 탭을 끌며 머묾(합치기 대기) · open: pane을 끌며 머묾(그 탭 열기 대기). */
  purpose: "merge" | "open";
}

/** 끌기가 일어나는 화면: 터미널 워크벤치(탭 바·pane) 또는 배치 편집(그룹 카드·블록·목록). */
export type LayoutDragArea = "workbench" | "editor";

export interface LayoutDragView {
  source: LayoutDragSource;
  area: LayoutDragArea;
  /** 끌고 있는 것의 이름(탭 이름·pane 제목). */
  title: string;
  x: number;
  y: number;
  resolution: LayoutDropResolution;
  /** 놓으면 바뀔 자리 — 아무 일도 없으면 null. */
  preview: LayoutDragPreview | null;
  /** 끌고 있는 탭·pane이 화면에 있으면 그 자리(흐리게 덮는다). */
  sourceRect: Rect | null;
  dwell: LayoutDragDwell | null;
}

export const useLayoutDragStore = create<{ view: LayoutDragView | null }>(() => ({ view: null }));

export type LayoutDragController = Pick<SessionController, "applyLayoutDrop" | "toast" | "focusPaneTerminal">;

interface HitResult {
  target: LayoutDropTarget | null;
  preview: LayoutDragPreview | null;
  dwell: LayoutDragDwell | null;
}

let current: DragSession | null = null;

/**
 * 탭·pane 헤더의 pointerdown에서 부른다. 주 버튼이 아니거나, 버튼·입력 칸 위에서
 * 눌렀거나, 모달이 떠 있거나, 이미 끌고 있으면 아무 일도 하지 않는다. 기본 동작은
 * 막지 않는다 — 임계값 전에 떼면 평소 클릭이어야 한다.
 */
export function startLayoutDrag(
  event: ReactPointerEvent<HTMLElement>,
  source: LayoutDragSource,
  controller: LayoutDragController,
  area: LayoutDragArea = "workbench",
): void {
  if (event.button !== 0 || !event.isPrimary || current !== null) return;
  const target = event.target;
  // 포털(헤더의 ⋯ 메뉴 등)에서 React 트리를 따라 올라온 누름은 손잡이 안의 누름이 아니다.
  if (!(target instanceof Node) || !event.currentTarget.contains(target)) return;
  if (target instanceof Element && target.closest(INTERACTIVE_SELECTOR)) return;
  if (useWorkbenchStore.getState().modal) return;
  current = new DragSession(event.nativeEvent, event.currentTarget, source, controller, area);
}

class DragSession {
  private started = false;
  private disposed = false;
  private readonly down: PointerEvent;
  private readonly handle: HTMLElement;
  private readonly source: LayoutDragSource;
  private readonly controller: LayoutDragController;
  private readonly area: LayoutDragArea;
  private x: number;
  private y: number;
  private frame: number | null = null;
  private dwellTabId: string | null = null;
  private dwellTimer: ReturnType<typeof setTimeout> | null = null;
  /** 머물러 합치기가 켜진 탭(탭을 끌 때) — 그 탭 가운데를 벗어나면 꺼진다. */
  private armedTabId: string | null = null;
  /** 머물러 다른 탭을 열기 전의 활성 탭·초점 — 아무 데도 놓지 않으면 되돌린다. */
  private origin: { activeTabId: string | null; focusedLeafId: string | null } | null = null;
  private unsubscribe: (() => void) | null = null;

  constructor(
    down: PointerEvent,
    handle: HTMLElement,
    source: LayoutDragSource,
    controller: LayoutDragController,
    area: LayoutDragArea,
  ) {
    this.down = down;
    this.handle = handle;
    this.source = source;
    this.controller = controller;
    this.area = area;
    this.x = down.clientX;
    this.y = down.clientY;
    // 이 누름 자체의 전파는 이미 window capture를 지났다 — 아래 pointerdown은 다음 누름만 받는다.
    window.addEventListener("pointerdown", this.onPointerDown, true);
    window.addEventListener("pointermove", this.onPointerMove, true);
    window.addEventListener("pointerup", this.onPointerUp, true);
    window.addEventListener("pointercancel", this.onPointerCancel, true);
    window.addEventListener("keydown", this.onKeyDown, true);
    window.addEventListener("blur", this.onBlur);
    // 아이콘·글자에서 웹뷰 기본 끌기(이미지 드래그)가 시작되면 pointercancel로 끊긴다.
    window.addEventListener("dragstart", preventDefault, true);
  }

  private readonly onPointerDown = (): void => {
    // 새 누름이 왔다 = 앞선 누름의 뗌을 놓쳤다(창 밖에서 뗌·시스템 제스처). 그 세션을 살려 두면
    // 다음 누름(터미널 글자 선택 등)의 움직임이 엉뚱한 탭·pane을 끌게 된다 — 여기서 끝낸다.
    this.cancel(false);
  };

  private readonly onPointerMove = (event: PointerEvent): void => {
    if (event.pointerId !== this.down.pointerId) return;
    this.x = event.clientX;
    this.y = event.clientY;
    if (!this.started) {
      // 창 밖에서 떼고 돌아온 움직임 — 누르고 있지 않으니 끌기가 아니다.
      if (event.buttons === 0) {
        this.dispose();
        return;
      }
      if (Math.hypot(this.x - this.down.clientX, this.y - this.down.clientY) < DRAG_THRESHOLD_PX) return;
      this.begin();
    } else if (event.buttons === 0) {
      // 창 밖에서 떼어 pointerup을 받지 못했다 — 놓은 자리를 알 수 없으니 취소한다.
      this.cancel(false);
      return;
    }
    // 지나가는 터미널(마우스 보고를 켠 TUI)로 움직임이 새지 않게 여기서 멈춘다.
    event.stopPropagation();
    this.schedule();
  };

  private readonly onPointerUp = (event: PointerEvent): void => {
    if (event.pointerId !== this.down.pointerId) return;
    if (!this.started) {
      this.dispose();
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    this.x = event.clientX;
    this.y = event.clientY;
    const state = useWorkbenchStore.getState();
    const resolution = resolveLayoutDrop(state.tabs, this.source, this.hitTest(state).target);
    const origin = this.origin;
    const releasedOnSourceTab =
      this.source.kind === "tab" && containsPoint(sourceRect(this.source, this.area), this.x, this.y);
    // 이 뗌에 곧바로 이어지는 mouseup·click(탭 선택 등)은 끌기의 끝이지 클릭이 아니다.
    swallowReleaseEvents();
    this.dispose();
    if (resolution.action !== null && this.controller.applyLayoutDrop(resolution.action)) {
      refocusTerminal(this.controller);
      return;
    }
    // 막힌 자리에 놓았으면 왜 안 되는지 알린다(실행 실패는 controller가 이미 알렸다).
    if (resolution.blocked !== null) this.controller.toast(layoutDropBlockText(resolution.blocked));
    restoreOrigin(origin);
    // 트랙패드에서 조금 흔들린 탭 클릭은 끌기로 시작돼도 아무 일 없이 제자리에서 끝난다 — 삼킨
    // click 대신 평소처럼 그 탭을 고른다. 배치 편집의 카드 머리는 눌러도 탭을 고르지 않으므로
    // (탭으로 가는 길은 "열기" 버튼이다) 흔들린 클릭도 아무 일이 없어야 한다.
    if (
      this.area === "workbench" &&
      resolution.action === null &&
      resolution.blocked === null &&
      releasedOnSourceTab &&
      this.source.kind === "tab"
    ) {
      useWorkbenchStore.getState().setActiveTab(this.source.tabId);
    }
    refocusTerminal(this.controller);
  };

  private readonly onPointerCancel = (event: PointerEvent): void => {
    if (event.pointerId === this.down.pointerId) this.cancel(false);
  };

  private readonly onKeyDown = (event: KeyboardEvent): void => {
    if (!this.started) return;
    // 끄는 동안의 키 입력은 터미널·앱 단축키로 보내지 않는다. Esc는 취소다.
    event.preventDefault();
    event.stopPropagation();
    if (event.key === "Escape") this.cancel(true);
  };

  private readonly onBlur = (): void => {
    this.cancel(false);
  };

  // 끄는 동안 휠로 보드·탭 바가 스크롤되면 포인터는 그대로여도 그 아래 자리가 바뀐다.
  private readonly onScroll = (): void => {
    this.schedule();
  };

  private begin(): void {
    this.started = true;
    try {
      this.handle.setPointerCapture(this.down.pointerId);
    } catch {
      // 요소가 이미 화면에서 빠졌으면 window 리스너만으로 따라간다.
    }
    document.body.classList.add("layout-dragging");
    window.getSelection()?.removeAllRanges();
    window.addEventListener("mousemove", stopPropagation, true);
    window.addEventListener("scroll", this.onScroll, true);
    // pane이 닫히거나 탭이 바뀌면(머물러 연 탭 포함) 놓을 자리를 다시 계산한다.
    this.unsubscribe = useWorkbenchStore.subscribe(() => this.schedule());
  }

  private schedule(): void {
    if (this.disposed || !this.started || this.frame !== null) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = null;
      try {
        this.update();
      } catch (error) {
        // 미리보기 계산이 깨져도 끌기 상태에 갇히지 않는다.
        console.error("layout drag update failed", error);
        this.cancel(false);
      }
    });
  }

  private update(): void {
    if (this.disposed) return;
    const state = useWorkbenchStore.getState();
    const title = sourceTitle(state, this.source);
    if (title === null) {
      // 끌던 탭·pane이 사라졌다(세션이 끝나 닫힘 등).
      this.cancel(false);
      return;
    }
    // 넘친 탭 바 끝에 머무르는 동안에는 프레임마다 조금씩 스크롤한다.
    if (this.area === "editor" ? autoScrollBoard(this.x, this.y) : autoScrollTabBar(this.x, this.y)) this.schedule();
    const hit = this.hitTest(state);
    const resolution = resolveLayoutDrop(state.tabs, this.source, hit.target);
    const willChange = resolution.action !== null || resolution.blocked !== null;
    document.body.classList.toggle("layout-drop-blocked", resolution.blocked !== null);
    useLayoutDragStore.setState({
      view: {
        source: this.source,
        area: this.area,
        title,
        x: this.x,
        y: this.y,
        resolution,
        preview: willChange ? hit.preview : null,
        sourceRect: sourceRect(this.source, this.area),
        dwell: hit.dwell,
      },
    });
  }

  private hitTest(state: WorkbenchState): HitResult {
    if (this.area === "editor") return hitEditor(state, this.source, this.x, this.y);
    const bar = readTabBar();
    if (bar && this.x >= bar.left && this.x <= bar.right && this.y >= bar.top && this.y <= bar.bottom) {
      return this.hitTabBar(bar, state);
    }
    this.trackDwell(null);
    this.armedTabId = null;
    const pane = paneAt(this.x, this.y);
    if (!pane) return { target: null, preview: null, dwell: null };
    const zone = paneZoneAt(pane.rect, this.x, this.y);
    return {
      target: { kind: "pane", leafId: pane.leafId, zone },
      preview: { kind: "zone", leafId: pane.leafId, zone, rect: clipRect(zonePreviewRect(pane.rect, zone), pane.clip) },
      dwell: null,
    };
  }

  private hitTabBar(bar: TabBarGeometry, state: WorkbenchState): HitResult {
    const raw = tabBarTargetAt(bar.slots, this.x, (tabId) => canDropOntoTab(state.tabs, this.source, tabId));
    const overTabId = raw.kind === "tab" ? raw.tabId : null;
    if (this.armedTabId !== overTabId) this.armedTabId = null;
    let target: LayoutDropTarget = raw;
    let dwell: LayoutDragDwell | null = null;
    if (overTabId === null) {
      this.trackDwell(null);
    } else {
      const merging = this.source.kind === "tab";
      // 탭을 끌며 지나가는 탭 위는 아직 순서 바꾸기다 — 머물러야 합치기가 켜진다.
      // pane을 끌 때 탭 위는 곧바로 "그 탭으로"이고, 머무르면 그 탭이 열린다.
      const waiting = merging ? this.armedTabId !== overTabId : overTabId !== state.activeTabId;
      this.trackDwell(waiting ? overTabId : null);
      const rect = bar.rects.get(overTabId);
      if (waiting && rect) {
        dwell = { tabId: overTabId, title: layoutTabName(state.tabs, overTabId), rect, purpose: merging ? "merge" : "open" };
      }
      if (merging && this.armedTabId !== overTabId) target = tabGapAt(bar.slots, this.x);
    }
    return { target, preview: tabBarPreview(bar, target), dwell };
  }

  private trackDwell(tabId: string | null): void {
    if (tabId === this.dwellTabId) return;
    if (this.dwellTimer !== null) clearTimeout(this.dwellTimer);
    this.dwellTimer = null;
    this.dwellTabId = tabId;
    if (tabId === null) return;
    this.dwellTimer = setTimeout(() => {
      this.dwellTimer = null;
      if (this.disposed || this.dwellTabId !== tabId) return;
      this.dwellTabId = null;
      if (this.source.kind === "tab") {
        this.armedTabId = tabId;
      } else {
        const store = useWorkbenchStore.getState();
        if (this.origin === null) this.origin = { activeTabId: store.activeTabId, focusedLeafId: store.focusedLeafId };
        // 누른 헤더가 곧 화면에서 빠진다 — 요소에 묶인 capture를 먼저 풀고 window 리스너로만 따라간다
        // (떼어진 요소에 capture를 남기는 웹뷰가 있으면 이후 움직임이 그 요소로 사라진다).
        this.releaseCapture();
        store.setActiveTab(tabId);
      }
      this.schedule();
    }, TAB_DWELL_MS);
  }

  /** 놓지 않고 끝낸다. 머물러 열었던 탭은 되돌린다. */
  private cancel(swallowRelease: boolean): void {
    if (this.disposed) return;
    const started = this.started;
    const origin = this.origin;
    this.dispose();
    if (!started) return;
    // Esc로 끝내도 버튼은 아직 눌려 있다 — 곧 올 뗌의 click이 탭을 고르지 않게 한다.
    if (swallowRelease) swallowReleaseEventsAfter(this.down.pointerId);
    restoreOrigin(origin);
    refocusTerminal(this.controller);
  }

  private releaseCapture(): void {
    try {
      if (this.handle.hasPointerCapture(this.down.pointerId)) this.handle.releasePointerCapture(this.down.pointerId);
    } catch {
      // 이미 풀렸다.
    }
  }

  private dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    window.removeEventListener("pointerdown", this.onPointerDown, true);
    window.removeEventListener("pointermove", this.onPointerMove, true);
    window.removeEventListener("pointerup", this.onPointerUp, true);
    window.removeEventListener("pointercancel", this.onPointerCancel, true);
    window.removeEventListener("keydown", this.onKeyDown, true);
    window.removeEventListener("blur", this.onBlur);
    window.removeEventListener("dragstart", preventDefault, true);
    window.removeEventListener("mousemove", stopPropagation, true);
    window.removeEventListener("scroll", this.onScroll, true);
    if (this.frame !== null) cancelAnimationFrame(this.frame);
    this.frame = null;
    if (this.dwellTimer !== null) clearTimeout(this.dwellTimer);
    this.dwellTimer = null;
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.releaseCapture();
    if (this.started) {
      document.body.classList.remove("layout-dragging", "layout-drop-blocked");
      useLayoutDragStore.setState({ view: null });
    }
    if (current === this) current = null;
  }
}

interface TabBarGeometry {
  left: number;
  right: number;
  top: number;
  bottom: number;
  slots: TabSlot[];
  rects: Map<string, Rect>;
  tabTop: number;
  tabHeight: number;
}

/** 탭 바의 화면 위치와 탭들의 가로 범위. 세로로는 상단 바 전체를 탭 바로 본다. */
function readTabBar(): TabBarGeometry | null {
  const bar = document.querySelector<HTMLElement>(".tab-bar");
  const header = bar?.closest<HTMLElement>(".top-bar") ?? null;
  if (!bar || !header) return null;
  const barRect = bar.getBoundingClientRect();
  const headerRect = header.getBoundingClientRect();
  const slots: TabSlot[] = [];
  const rects = new Map<string, Rect>();
  let tabTop = headerRect.top + 6;
  let tabHeight = Math.max(0, headerRect.height - 12);
  for (const element of Array.from(bar.querySelectorAll<HTMLElement>("[data-tab-id]"))) {
    const tabId = element.dataset.tabId;
    if (!tabId) continue;
    const rect = toRect(element.getBoundingClientRect());
    slots.push({ tabId, left: rect.left, right: rect.left + rect.width });
    rects.set(tabId, rect);
    tabTop = rect.top;
    tabHeight = rect.height;
  }
  return {
    left: barRect.left,
    right: barRect.right,
    top: headerRect.top,
    bottom: headerRect.bottom,
    slots,
    rects,
    tabTop,
    tabHeight,
  };
}

function tabBarPreview(bar: TabBarGeometry, target: LayoutDropTarget): LayoutDragPreview | null {
  if (target.kind === "tab") {
    const rect = bar.rects.get(target.tabId);
    return rect ? { kind: "tab", tabId: target.tabId, rect } : null;
  }
  if (target.kind !== "tab-gap") return null;
  return { kind: "insert", x: tabGapX(bar.slots, target.index) ?? bar.left + 4, top: bar.tabTop, height: bar.tabHeight };
}

/** 포인터 아래의 pane과, 그 pane이 보이는 영역(넘치면 스크롤되는 분할 영역). */
function paneAt(x: number, y: number): { leafId: string; rect: Rect; clip: Rect | null } | null {
  const element = document.elementFromPoint(x, y)?.closest<HTMLElement>(".terminal-pane[data-leaf-id]") ?? null;
  const leafId = element?.dataset.leafId;
  if (!element || !leafId) return null;
  const scroller = element.closest<HTMLElement>(".split-scroll");
  return {
    leafId,
    rect: toRect(element.getBoundingClientRect()),
    clip: scroller ? toRect(scroller.getBoundingClientRect()) : null,
  };
}

function sourceRect(source: LayoutDragSource, area: LayoutDragArea): Rect | null {
  const selector =
    source.kind === "session"
      ? `[data-layout-roster-session="${cssEscape(source.sessionId)}"]`
      : area === "editor"
        ? source.kind === "tab"
          ? `.layout-board [data-layout-tab-id="${cssEscape(source.tabId)}"]`
          : `.layout-board [data-layout-leaf-id="${cssEscape(source.leafId)}"]`
        : source.kind === "tab"
          ? `.tab-bar [data-tab-id="${cssEscape(source.tabId)}"]`
          : `.terminal-pane[data-leaf-id="${cssEscape(source.leafId)}"]`;
  const element = document.querySelector<HTMLElement>(selector);
  return element ? toRect(element.getBoundingClientRect()) : null;
}

/** 배치 편집 보드 위의 놓을 자리: "+ 새 그룹" 카드 · 블록 가장자리/가운데 · 그룹 카드. */
function hitEditor(state: WorkbenchState, source: LayoutDragSource, x: number, y: number): HitResult {
  const none: HitResult = { target: null, preview: null, dwell: null };
  const element = document.elementFromPoint(x, y);
  const board = element?.closest<HTMLElement>(".layout-board") ?? null;
  if (!element || !board) return none;
  const clip = toRect(board.getBoundingClientRect());
  const newGroup = element.closest<HTMLElement>("[data-layout-new-group]");
  if (newGroup) {
    return {
      target: { kind: "tab-gap", index: state.tabs.length },
      preview: { kind: "tab", tabId: "new-group", rect: clipRect(toRect(newGroup.getBoundingClientRect()), clip) },
      dwell: null,
    };
  }
  // 카드를 끌 때는 블록도 그 카드의 일부다 — 순서 자리만 고른다.
  const blockElement = source.kind === "tab" ? null : element.closest<HTMLElement>("[data-layout-leaf-id]");
  const leafId = blockElement?.dataset.layoutLeafId;
  if (blockElement && leafId) {
    const rect = toRect(blockElement.getBoundingClientRect());
    const zone: PaneZone = source.kind === "session" ? attachZoneAt(rect, x, y) : paneZoneAt(rect, x, y);
    return {
      target: { kind: "pane", leafId, zone },
      preview: { kind: "zone", leafId, zone, rect: clipRect(zonePreviewRect(rect, zone), clip) },
      dwell: null,
    };
  }
  const card = element.closest<HTMLElement>("[data-layout-tab-id]");
  const tabId = card?.dataset.layoutTabId;
  if (!card || !tabId) return none;
  const rect = toRect(card.getBoundingClientRect());
  if (source.kind === "tab") {
    const index = state.tabs.findIndex((tab) => tab.id === tabId);
    if (index < 0) return none;
    const before = x < rect.left + rect.width / 2;
    return {
      target: { kind: "tab-gap", index: before ? index : index + 1 },
      preview: {
        kind: "insert",
        x: before ? rect.left - CARD_GAP_PX / 2 : rect.left + rect.width + CARD_GAP_PX / 2,
        // 보드 밖(스크롤로 가려진 부분)으로 막대가 삐져나오지 않게 보이는 영역으로 자른다.
        top: Math.max(rect.top, clip.top),
        height: Math.max(0, Math.min(rect.top + rect.height, clip.top + clip.height) - Math.max(rect.top, clip.top)),
      },
      dwell: null,
    };
  }
  return { target: { kind: "tab", tabId }, preview: { kind: "tab", tabId, rect: clipRect(rect, clip) }, dwell: null };
}

/** 배치 편집 보드가 넘치면 위·아래 끝 가까이에 머무르는 동안 저절로 스크롤한다. */
function autoScrollBoard(x: number, y: number): boolean {
  const board = document.querySelector<HTMLElement>(".layout-board");
  if (!board || board.scrollHeight <= board.clientHeight) return false;
  const rect = board.getBoundingClientRect();
  if (x < rect.left || x > rect.right) return false;
  let delta = 0;
  if (y < rect.top + AUTOSCROLL_EDGE_PX && y >= rect.top - AUTOSCROLL_EDGE_PX) delta = -AUTOSCROLL_STEP_PX;
  else if (y > rect.bottom - AUTOSCROLL_EDGE_PX && y <= rect.bottom + AUTOSCROLL_EDGE_PX) delta = AUTOSCROLL_STEP_PX;
  if (delta === 0) return false;
  const before = board.scrollTop;
  board.scrollTop = before + delta;
  return board.scrollTop !== before;
}

/** 끌고 있는 것의 이름. 그것이 이미 사라졌으면 null. */
function sourceTitle(state: WorkbenchState, source: LayoutDragSource): string | null {
  if (source.kind === "tab") {
    return state.tabs.some((tab) => tab.id === source.tabId) ? layoutTabName(state.tabs, source.tabId) : null;
  }
  if (source.kind === "session") {
    // 끄는 사이 다른 곳에서 붙었거나 끝난 세션은 더 끌 것이 없다.
    const attached = Object.values(state.panes).some((pane) => pane.sessionId === source.sessionId);
    const workload = state.workloadBySession.get(source.sessionId);
    // 배치 안 됨 목록과 같은 이름(창을 닫기 전 마지막 제목)으로 부른다.
    return !attached && workload && (workload.state === "RUNNING" || workload.state === "STARTING")
      ? workloadListTitle(workload, state.panes, state.workloadMemory)
      : null;
  }
  const pane = state.panes[source.leafId];
  const placed = state.tabs.some((tab) => tab.kind === "terminal" && findLeaf(tab.root, source.leafId) !== null);
  return pane && placed ? terminalDisplayTitle(pane.title) : null;
}

function autoScrollTabBar(x: number, y: number): boolean {
  const bar = document.querySelector<HTMLElement>(".tab-bar");
  const header = bar?.closest<HTMLElement>(".top-bar") ?? null;
  if (!bar || !header || bar.scrollWidth <= bar.clientWidth) return false;
  const barRect = bar.getBoundingClientRect();
  const headerRect = header.getBoundingClientRect();
  if (y < headerRect.top || y > headerRect.bottom) return false;
  let delta = 0;
  if (x < barRect.left + AUTOSCROLL_EDGE_PX) delta = -AUTOSCROLL_STEP_PX;
  else if (x > barRect.right - AUTOSCROLL_EDGE_PX && x <= barRect.right) delta = AUTOSCROLL_STEP_PX;
  if (delta === 0) return false;
  const before = bar.scrollLeft;
  bar.scrollLeft = before + delta;
  return bar.scrollLeft !== before;
}

/** 머물러 열었던 탭을 되돌린다(아무 데도 놓지 않았을 때). */
function restoreOrigin(origin: { activeTabId: string | null; focusedLeafId: string | null } | null): void {
  if (!origin?.activeTabId) return;
  const store = useWorkbenchStore.getState();
  const tab = store.tabs.find((candidate) => candidate.id === origin.activeTabId);
  if (!tab) return;
  store.setActiveTab(tab.id);
  if (origin.focusedLeafId && tab.kind === "terminal" && findLeaf(tab.root, origin.focusedLeafId)) {
    useWorkbenchStore.getState().focusPane(origin.focusedLeafId);
  }
}

/** 탭을 누르며 옮겨 간 키보드 초점을 보고 있던 터미널로 돌려준다(화면이 다시 붙은 뒤). */
function refocusTerminal(controller: LayoutDragController): void {
  requestAnimationFrame(() => {
    const store = useWorkbenchStore.getState();
    // 배치 편집 등 다른 화면이 떠 있으면 터미널은 숨어 있다 — 돌아올 때 워크벤치가 초점을 맞춘다.
    if (store.modal || store.page !== "terminal" || !store.focusedLeafId) return;
    controller.focusPaneTerminal(store.focusedLeafId);
  });
}

/** 끌기를 끝낸 뗌이 곧바로 만드는 mouseup·click을 한 번 삼킨다. */
function swallowReleaseEvents(): void {
  const swallow = (event: Event) => {
    event.preventDefault();
    event.stopPropagation();
  };
  window.addEventListener("mouseup", swallow, true);
  window.addEventListener("click", swallow, true);
  // 둘 다 같은 뗌에 이어 곧바로 온다 — 오지 않았으면(다른 요소 위에서 뗌) 다음 틱에 거둔다.
  setTimeout(() => {
    window.removeEventListener("mouseup", swallow, true);
    window.removeEventListener("click", swallow, true);
  }, 0);
}

/** 아직 누르고 있는 버튼을 뗄 때의 mouseup·click을 삼킨다(Esc 취소 뒤). */
function swallowReleaseEventsAfter(pointerId: number): void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const stop = (): void => {
    window.removeEventListener("pointerup", onRelease, true);
    window.removeEventListener("pointerdown", stop, true);
    if (timer !== undefined) clearTimeout(timer);
  };
  const onRelease = (event: PointerEvent): void => {
    if (event.pointerId !== pointerId) return;
    stop();
    swallowReleaseEvents();
  };
  window.addEventListener("pointerup", onRelease, true);
  // 뗌보다 새 누름이 먼저 오면 앞선 뗌은 이미 사라진 것이다 — 그 새 클릭을 삼키지 않게 거둔다.
  window.addEventListener("pointerdown", stop, true);
  // 뗌이 끝내 오지 않으면(창 밖에서 뗌) 나중의 평범한 클릭을 삼키지 않게 거둔다.
  timer = setTimeout(stop, 5_000);
}

function containsPoint(rect: Rect | null, x: number, y: number): boolean {
  return rect !== null && x >= rect.left && x <= rect.left + rect.width && y >= rect.top && y <= rect.top + rect.height;
}

function preventDefault(event: Event): void {
  event.preventDefault();
}

function stopPropagation(event: Event): void {
  event.stopPropagation();
}

function toRect(rect: DOMRect): Rect {
  return { left: rect.left, top: rect.top, width: rect.width, height: rect.height };
}

function clipRect(rect: Rect, clip: Rect | null): Rect {
  if (!clip) return rect;
  const left = Math.max(rect.left, clip.left);
  const top = Math.max(rect.top, clip.top);
  const right = Math.min(rect.left + rect.width, clip.left + clip.width);
  const bottom = Math.min(rect.top + rect.height, clip.top + clip.height);
  return { left, top, width: Math.max(0, right - left), height: Math.max(0, bottom - top) };
}

function cssEscape(value: string): string {
  return typeof CSS !== "undefined" && typeof CSS.escape === "function"
    ? CSS.escape(value)
    : value.replace(/["\\]/g, "\\$&");
}
