/**
 * 탭 전환 방향 판정(tabSwitchEffect.ts).
 */

import { describe, expect, it } from "vitest";
import { TAB_SWITCH_DURATION_MS, tabSwitchDirection } from "./tabSwitchEffect";

describe("tabSwitchDirection", () => {
  it("오른쪽 탭으로 가면 next", () => {
    expect(tabSwitchDirection(0, 1)).toBe("next");
    expect(tabSwitchDirection(1, 3)).toBe("next");
  });

  it("왼쪽 탭으로 가면 prev", () => {
    expect(tabSwitchDirection(2, 0)).toBe("prev");
    expect(tabSwitchDirection(3, 1)).toBe("prev");
  });

  it("첫 렌더(prevIndex === null)에는 애니메이션을 걸지 않는다", () => {
    expect(tabSwitchDirection(null, 0)).toBeNull();
    expect(tabSwitchDirection(null, 2)).toBeNull();
  });

  it("같은 탭이면 null이다", () => {
    expect(tabSwitchDirection(1, 1)).toBeNull();
  });

  it("대상을 못 찾으면(nextIndex < 0) null이다", () => {
    expect(tabSwitchDirection(0, -1)).toBeNull();
  });
});

describe("TAB_SWITCH_DURATION_MS", () => {
  it("빠릿한 150ms다", () => {
    expect(TAB_SWITCH_DURATION_MS).toBe(150);
  });
});
