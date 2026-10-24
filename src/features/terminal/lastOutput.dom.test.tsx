/**
 * 마지막 출력 배지의 공유 티커(lastOutput.ts) — 창이 숨어 있는 동안은 돌지
 * 않고, 다시 보이면 곧바로 한 번 맞춘 뒤 다시 돈다. 구독자가 모두 빠지면
 * 티커와 표시 관찰자를 함께 끈다(happy-dom: document.hidden을 바꿔 친다).
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { NOW_TICK_MS, subscribeNow, useNow } from "./lastOutput";

let hidden = false;

function setHidden(next: boolean): void {
  hidden = next;
  document.dispatchEvent(new Event("visibilitychange"));
}

beforeEach(() => {
  vi.useFakeTimers();
  hidden = false;
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
});

afterEach(() => {
  // 문서 원래의 hidden(프로토타입 getter)으로 되돌린다.
  delete (document as unknown as { hidden?: boolean }).hidden;
  vi.useRealTimers();
});

describe("공유 티커 — 숨은 창에서는 멈춘다", () => {
  it("숨어 있는 동안은 틱이 없고, 보이면 곧바로 한 번 맞춘 뒤 다시 돈다", () => {
    const listener = vi.fn();
    const stop = subscribeNow(listener);
    try {
      vi.advanceTimersByTime(NOW_TICK_MS);
      expect(listener).toHaveBeenCalledTimes(1);

      setHidden(true);
      // 인터벌 자체를 끈다 — 틱을 건너뛰는 것이 아니라 타이머가 없다.
      expect(vi.getTimerCount()).toBe(0);
      vi.advanceTimersByTime(NOW_TICK_MS * 6);
      expect(listener).toHaveBeenCalledTimes(1);

      setHidden(false);
      expect(listener).toHaveBeenCalledTimes(2); // 돌아오자마자 한 번
      vi.advanceTimersByTime(NOW_TICK_MS);
      expect(listener).toHaveBeenCalledTimes(3); // 그리고 다시 5초마다
    } finally {
      stop();
    }
  });

  it("숨은 채로 구독을 시작하면 타이머를 켜지 않고, 보일 때 시작한다", () => {
    setHidden(true);
    const listener = vi.fn();
    const stop = subscribeNow(listener);
    try {
      expect(vi.getTimerCount()).toBe(0);
      vi.advanceTimersByTime(NOW_TICK_MS * 3);
      expect(listener).not.toHaveBeenCalled();
      setHidden(false);
      expect(listener).toHaveBeenCalledTimes(1);
      expect(vi.getTimerCount()).toBe(1);
    } finally {
      stop();
    }
  });

  it("구독자들이 타이머 하나를 나눠 쓰고, 마지막 구독자가 빠지면 티커와 표시 관찰자를 끈다", () => {
    const first = vi.fn();
    const second = vi.fn();
    const stopFirst = subscribeNow(first);
    const stopSecond = subscribeNow(second);
    expect(vi.getTimerCount()).toBe(1);
    vi.advanceTimersByTime(NOW_TICK_MS);
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledTimes(1);

    stopFirst();
    expect(vi.getTimerCount()).toBe(1); // 아직 구독자가 남아 있다
    stopSecond();
    expect(vi.getTimerCount()).toBe(0);

    // 표시 관찰자도 떼어졌다 — 창이 다시 보여도 아무도 깨우지 않고 타이머도 켜지 않는다.
    setHidden(true);
    setHidden(false);
    expect(vi.getTimerCount()).toBe(0);
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledTimes(1);
  });
});

describe("useNow", () => {
  let root: Root | null = null;
  let container: HTMLElement | null = null;

  afterEach(() => {
    act(() => root?.unmount());
    container?.remove();
    root = null;
    container = null;
  });

  function Clock(): JSX.Element {
    return <span>{useNow()}</span>;
  }

  it("공유 티커가 돌 때 다시 그리고, 숨은 동안에는 그대로 둔다", () => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    act(() => root?.render(<Clock />));
    const initial = container.textContent;

    act(() => { vi.advanceTimersByTime(NOW_TICK_MS); });
    const ticked = container.textContent;
    expect(ticked).not.toBe(initial);

    act(() => { setHidden(true); });
    act(() => { vi.advanceTimersByTime(NOW_TICK_MS * 4); });
    expect(container.textContent).toBe(ticked);

    act(() => { setHidden(false); });
    expect(container.textContent).not.toBe(ticked);

    act(() => root?.unmount());
    root = null;
    expect(vi.getTimerCount()).toBe(0);
  });
});
