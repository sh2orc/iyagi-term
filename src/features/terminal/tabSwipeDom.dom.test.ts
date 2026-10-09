// @vitest-environment happy-dom
/**
 * 트랙패드 제스처의 DOM 층(tabSwipeDom.ts): 언제 wheel을 가져가고 언제
 * 그대로 흘려보내는가.
 *
 * 터미널 위에서도 가로 제스처는 탭을 넘긴다(capture 리스너가 xterm보다 먼저
 * 본다). 흘려보내야 하는 것은 세로 스크롤과 가로로 스크롤되는 목록(탭 바)뿐이다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { attachTabSwipe } from "./tabSwipeDom";
import { DEFAULT_TAB_SWIPE, type TabSwipePrefs } from "./tabSwipe";

interface Harness {
  root: HTMLElement;
  /** 보통 pane의 xterm 화면. */
  screen: HTMLElement;
  /** 마우스 보고 중인 TUI(Claude·Codex 등) pane의 화면 — 이제 여기서도 먹힌다. */
  terminalScreen: HTMLElement;
  /** 가로로 스크롤되는 탭 막대 안의 탭. */
  tab: HTMLElement;
  cycle: ReturnType<typeof vi.fn>;
  stop: () => void;
  prefs: TabSwipePrefs;
}

function harness(overrides: Partial<TabSwipePrefs> = {}): Harness {
  const root = document.createElement("div");
  root.className = "workbench";

  const tabBar = document.createElement("nav");
  tabBar.className = "tab-bar";
  tabBar.setAttribute("style", "overflow-x: auto");
  // 탭이 넘쳐 실제로 가로 스크롤되는 상태.
  Object.defineProperty(tabBar, "scrollWidth", { value: 900, configurable: true });
  Object.defineProperty(tabBar, "clientWidth", { value: 400, configurable: true });
  const tab = document.createElement("button");
  tabBar.appendChild(tab);

  const pane = document.createElement("section");
  pane.className = "terminal-pane";
  pane.dataset.leafId = "leaf-1";
  const screen = document.createElement("div");
  screen.className = "xterm";
  pane.appendChild(screen);

  const terminal = document.createElement("section");
  terminal.className = "terminal-pane";
  terminal.dataset.leafId = "leaf-tui";
  const terminalScreen = document.createElement("div");
  terminalScreen.className = "xterm";
  terminal.appendChild(terminalScreen);

  root.append(tabBar, pane, terminal);
  document.body.appendChild(root);

  const harnessPrefs: TabSwipePrefs = { ...DEFAULT_TAB_SWIPE, ...overrides };
  const cycle = vi.fn(() => true);
  const stop = attachTabSwipe(root, { prefs: () => harnessPrefs, cycle });
  return { root, screen, terminalScreen, tab, cycle, stop, prefs: harnessPrefs };
}

function wheel(
  target: Element,
  init: { deltaX?: number; deltaY?: number; deltaMode?: number; at?: number },
): WheelEvent {
  const event = new WheelEvent("wheel", {
    deltaX: init.deltaX ?? 0,
    deltaY: init.deltaY ?? 0,
    deltaMode: init.deltaMode ?? 0,
    bubbles: true,
    cancelable: true,
  });
  Object.defineProperty(event, "timeStamp", { value: init.at ?? 0, configurable: true });
  target.dispatchEvent(event);
  return event;
}

afterEach(() => {
  document.body.replaceChildren();
});

describe("워크벤치 루트의 wheel 가로채기", () => {
  it("step을 넘은 가로 제스처가 탭을 넘기고 이벤트를 소비한다", () => {
    const { screen, cycle, stop } = harness({ sensitivity: "medium" }); // step 78
    const first = wheel(screen, { deltaX: 60, at: 0 });
    expect(first.defaultPrevented).toBe(true); // 데드존을 넘은 제스처 진행 중
    expect(cycle).not.toHaveBeenCalled();
    const fire = wheel(screen, { deltaX: 60, at: 16 });
    expect(fire.defaultPrevented).toBe(true);
    expect(cycle).toHaveBeenCalledTimes(1);
    expect(cycle).toHaveBeenCalledWith("next");
    stop();
  });

  it("마우스 보고 중인 터미널(Claude·Codex) 위에서도 가로 제스처가 탭을 넘긴다", () => {
    const { terminalScreen, cycle, stop } = harness({ sensitivity: "high" }); // step 48
    const fire = wheel(terminalScreen, { deltaX: 60, at: 0 });
    expect(fire.defaultPrevented).toBe(true);
    expect(cycle).toHaveBeenCalledWith("next");
    stop();
  });

  it("터미널 위에서도 세로 스크롤은 앱(터미널)에 그대로 넘긴다", () => {
    const { terminalScreen, cycle, stop } = harness({ sensitivity: "high" });
    const event = wheel(terminalScreen, { deltaY: 200, at: 0 });
    expect(event.defaultPrevented).toBe(false);
    expect(cycle).not.toHaveBeenCalled();
    stop();
  });

  it("세로 스크롤은 건드리지 않는다", () => {
    const { screen, cycle, stop } = harness();
    const event = wheel(screen, { deltaY: 200, at: 0 });
    expect(event.defaultPrevented).toBe(false);
    expect(cycle).not.toHaveBeenCalled();
    stop();
  });

  it("소비한 이벤트는 xterm까지 내려가지 않는다(capture + stopPropagation)", () => {
    const { screen, stop } = harness({ sensitivity: "high" });
    const seen = vi.fn();
    screen.addEventListener("wheel", seen);
    wheel(screen, { deltaX: 80, at: 0 });
    expect(seen).not.toHaveBeenCalled();
    wheel(screen, { deltaY: 80, at: 1000 });
    expect(seen).toHaveBeenCalledTimes(1); // 세로는 그대로 통과한다
    stop();
  });

  it("한 스와이프의 관성 꼬리를 소비하고 다음 스와이프에서만 다시 넘긴다", () => {
    const { screen, cycle, stop } = harness({ sensitivity: "high" });
    expect(wheel(screen, { deltaX: 60, at: 0 }).defaultPrevented).toBe(true);
    expect(cycle).toHaveBeenCalledTimes(1);
    for (let at = 16; at <= 1600; at += 16) {
      expect(wheel(screen, { deltaX: 60, at }).defaultPrevented, `hold@${at}`).toBe(true);
    }
    expect(wheel(screen, { deltaX: 1, at: 1700 }).defaultPrevented).toBe(true);
    expect(cycle).toHaveBeenCalledTimes(1);
    expect(wheel(screen, { deltaX: -60, at: 1900 }).defaultPrevented).toBe(true);
    expect(cycle).toHaveBeenCalledTimes(2);
    expect(cycle).toHaveBeenLastCalledWith("prev");
    stop();
  });

  it("관성이 남아 있어도 다시 미는 입력은 다음 탭으로 전환한다", () => {
    const { screen, cycle, stop } = harness({ sensitivity: "high" });
    wheel(screen, { deltaX: 60, at: 0 });
    [40, 20, 5, 1].forEach((deltaX, index) => {
      expect(wheel(screen, { deltaX, at: (index + 1) * 16 }).defaultPrevented).toBe(true);
    });
    expect(cycle).toHaveBeenCalledTimes(1);
    wheel(screen, { deltaX: 20, at: 80 });
    wheel(screen, { deltaX: 30, at: 96 });
    expect(cycle).toHaveBeenCalledTimes(2);
    expect(cycle).toHaveBeenLastCalledWith("next");
    wheel(screen, { deltaX: -60, at: 112 });
    expect(cycle).toHaveBeenCalledTimes(3);
    expect(cycle).toHaveBeenLastCalledWith("prev");
    stop();
  });

  it("가로로 스크롤되는 조상(탭 막대) 위에서는 그 스크롤이 임자다", () => {
    const { tab, cycle, stop } = harness({ sensitivity: "high" });
    const event = wheel(tab, { deltaX: 300, at: 0 });
    expect(event.defaultPrevented).toBe(false);
    expect(cycle).not.toHaveBeenCalled();
    stop();
  });

  it("가로로 넘치기만 하고 스크롤되지 않는 조상(overflow hidden)은 막지 않는다", () => {
    const { screen, cycle, stop } = harness({ sensitivity: "high" });
    const pane = screen.parentElement!;
    pane.setAttribute("style", "overflow-x: hidden");
    Object.defineProperty(pane, "scrollWidth", { value: 900, configurable: true });
    Object.defineProperty(pane, "clientWidth", { value: 400, configurable: true });
    expect(wheel(screen, { deltaX: 300, at: 0 }).defaultPrevented).toBe(true);
    expect(cycle).toHaveBeenCalledWith("next");
    stop();
  });

  it("설정이 꺼져 있으면 아무것도 하지 않는다", () => {
    const { screen, cycle, stop } = harness({ enabled: false });
    const event = wheel(screen, { deltaX: 400, at: 0 });
    expect(event.defaultPrevented).toBe(false);
    expect(cycle).not.toHaveBeenCalled();
    stop();
  });

  it("감도 설정이 step을 정한다(낮음은 같은 거리로 넘어가지 않는다)", () => {
    const low = harness({ sensitivity: "low" }); // step 130
    wheel(low.screen, { deltaX: 120, at: 0 });
    expect(low.cycle).not.toHaveBeenCalled();
    low.stop();

    const high = harness({ sensitivity: "high" }); // step 48
    wheel(high.screen, { deltaX: 120, at: 0 });
    expect(high.cycle).toHaveBeenCalledWith("next");
    high.stop();
  });

  it("방향 반전 설정을 이벤트마다 읽는다", () => {
    const { screen, cycle, prefs, stop } = harness({ sensitivity: "high" });
    wheel(screen, { deltaX: 80, at: 0 });
    expect(cycle).toHaveBeenLastCalledWith("next");
    prefs.reverse = true;
    wheel(screen, { deltaX: 80, at: 1000 });
    expect(cycle).toHaveBeenLastCalledWith("prev");
    stop();
  });

  it("해제하면 리스너가 사라진다", () => {
    const { screen, cycle, stop } = harness({ sensitivity: "high" });
    stop();
    const event = wheel(screen, { deltaX: 300, at: 0 });
    expect(event.defaultPrevented).toBe(false);
    expect(cycle).not.toHaveBeenCalled();
  });
});
