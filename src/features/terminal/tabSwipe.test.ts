/**
 * 트랙패드 스와이프 인식기 계약(tabSwipe.ts).
 *
 * 한 제스처당 한 탭, 관성 소비, 새 제스처 리셋과 세로 스크롤 보존을 확인한다.
 */

import { describe, expect, it } from "vitest";
import {
  TAB_SWIPE_DEAD_ZONE_PX,
  TAB_SWIPE_GESTURE_RESET_MS,
  TAB_SWIPE_STEP_PX,
  TabSwipeRecognizer,
  tabSwipeStep,
  type TabSwipeWheelLike,
} from "./tabSwipe";

/** 60Hz 트랙패드 흐름을 흉내 낸다 — 시각은 호출자가 명시한다. */
function wheel(patch: Partial<TabSwipeWheelLike> = {}): TabSwipeWheelLike {
  return { deltaX: 0, deltaY: 0, deltaMode: 0, timeStamp: 0, ...patch };
}

describe("감도 → step(px)", () => {
  it("낮음 130 · 보통 78 · 높음 48", () => {
    expect(TAB_SWIPE_STEP_PX).toEqual({ low: 130, medium: 78, high: 48 });
    expect(tabSwipeStep("low")).toBe(130);
    expect(tabSwipeStep("medium")).toBe(78);
    expect(tabSwipeStep("high")).toBe(48);
  });

  it("새 제스처 간격 200ms", () => {
    expect(TAB_SWIPE_GESTURE_RESET_MS).toBe(200);
  });
});

describe("step 발동", () => {
  it("누적이 step을 넘을 때 발동한다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 80 });
    expect(r.feed(wheel({ deltaX: 25, timeStamp: 0 }))).toBeNull(); // 25
    expect(r.feed(wheel({ deltaX: 25, timeStamp: 16 }))).toBeNull(); // 50
    expect(r.feed(wheel({ deltaX: 25, timeStamp: 32 }))).toBeNull(); // 75
    expect(r.feed(wheel({ deltaX: 25, timeStamp: 48 }))).toBe("next"); // 100 ≥ 80
  });

  it("step과 정확히 같은 누적에서 발동한다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 60 });
    expect(r.feed(wheel({ deltaX: 60, timeStamp: 0 }))).toBe("next");
  });

  it("반대 방향이면 prev다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 60 });
    expect(r.feed(wheel({ deltaX: -80, timeStamp: 0 }))).toBe("prev");
  });
});

describe("한 제스처당 한 탭", () => {
  it.each([60, -60])("긴 스와이프와 관성 꼬리는 한 번만 넘긴다: %s", (deltaX) => {
    const r = new TabSwipeRecognizer({ stepPx: 60 });
    expect(r.feed(wheel({ deltaX, timeStamp: 0 }))).toBe(deltaX > 0 ? "next" : "prev");
    for (let at = 16; at <= 1600; at += 16) {
      expect(r.feed(wheel({ deltaX: deltaX * 10, timeStamp: at }))).toBeNull();
      expect(r.consumedLastEvent).toBe(true);
    }
    expect(r.feed(wheel({ deltaX: Math.sign(deltaX), timeStamp: 1700 }))).toBeNull();
    expect(r.consumedLastEvent).toBe(true);
    expect(r.feed(wheel({ deltaX, timeStamp: 1900 }))).toBe(deltaX > 0 ? "next" : "prev");
  });

  it("세로 이벤트와 설정 변경은 같은 방향의 관성 잠금을 풀지 않는다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 60 });
    expect(r.feed(wheel({ deltaX: 60, timeStamp: 0 }))).toBe("next");
    expect(r.feed(wheel({ deltaY: 80, timeStamp: 100 }))).toBeNull();
    expect(r.consumedLastEvent).toBe(false);
    r.configure({ stepPx: 48, reverse: true });
    expect(r.feed(wheel({ deltaX: 600, timeStamp: 200 }))).toBeNull();
    expect(r.consumedLastEvent).toBe(true);
  });

  it.each([1, -1])("관성 중 다시 같은 방향으로 밀면 기다리지 않고 넘긴다: %s", (sign) => {
    const r = new TabSwipeRecognizer({ stepPx: 60 });
    const direction = sign > 0 ? "next" : "prev";
    expect(r.feed(wheel({ deltaX: sign * 60, timeStamp: 0 }))).toBe(direction);
    [50, 30, 15, 6, 2].forEach((delta, index) => {
      expect(r.feed(wheel({ deltaX: sign * delta, timeStamp: (index + 1) * 16 }))).toBeNull();
    });
    expect(r.feed(wheel({ deltaX: sign * 10, timeStamp: 96 }))).toBeNull();
    expect(r.feed(wheel({ deltaX: sign * 25, timeStamp: 112 }))).toBeNull();
    expect(r.feed(wheel({ deltaX: sign * 30, timeStamp: 128 }))).toBe(direction);
    expect(r.feed(wheel({ deltaX: sign * 40, timeStamp: 144 }))).toBeNull();
  });

  it("반대 방향으로 다시 밀면 관성이 끝나기 전에 돌아간다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 60 });
    expect(r.feed(wheel({ deltaX: 60, timeStamp: 0 }))).toBe("next");
    expect(r.feed(wheel({ deltaX: -20, timeStamp: 48 }))).toBeNull();
    expect(r.feed(wheel({ deltaX: -40, timeStamp: 64 }))).toBe("prev");
  });

  it("작은 관성 흔들림이나 반대 방향 꼬리로는 다시 넘기지 않는다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 60 });
    expect(r.feed(wheel({ deltaX: 60, timeStamp: 0 }))).toBe("next");
    [30, 10, 2, 3, 1, -2, 2, 1].forEach((deltaX, index) => {
      expect(r.feed(wheel({ deltaX, timeStamp: (index + 1) * 16 }))).toBeNull();
      expect(r.consumedLastEvent).toBe(true);
    });
  });

  it("이벤트가 200ms 끊기면 감속 없이도 새 스와이프를 받는다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 60, gestureResetMs: 200 });
    expect(r.feed(wheel({ deltaX: 60, timeStamp: 0 }))).toBe("next");
    expect(r.feed(wheel({ deltaX: 60, timeStamp: 199 }))).toBeNull();
    expect(r.feed(wheel({ deltaX: 60, timeStamp: 399 }))).toBe("next");
  });

  it("오래 쉰 뒤의 이벤트는 앞선 누적을 물려받지 않는다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 120 });
    expect(r.feed(wheel({ deltaX: 100, timeStamp: 0 }))).toBeNull();
    expect(r.feed(wheel({ deltaX: 100, timeStamp: 5000 }))).toBeNull(); // 새 제스처 — 100뿐
    expect(r.consumedLastEvent).toBe(true); // 데드존은 넘었다(제스처 진행 중)
  });
});

describe("방향 전환", () => {
  it("deltaX 부호가 바뀌면 누적을 버린다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 120 });
    expect(r.feed(wheel({ deltaX: 100, timeStamp: 0 }))).toBeNull();
    // 부호가 바뀌면 +100은 사라지고 누적은 -100에서 다시 센다.
    expect(r.feed(wheel({ deltaX: -100, timeStamp: 16 }))).toBeNull();
    expect(r.feed(wheel({ deltaX: -10, timeStamp: 32 }))).toBeNull(); // -110
    expect(r.feed(wheel({ deltaX: -30, timeStamp: 48 }))).toBe("prev"); // -140
  });
});

describe("세로 우위", () => {
  it("|deltaX| < 2·|deltaY|면 스크롤이다 — 누적을 버리고 소비하지 않는다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 120 });
    expect(r.feed(wheel({ deltaX: 100, timeStamp: 0 }))).toBeNull();
    expect(r.feed(wheel({ deltaX: 10, deltaY: 60, timeStamp: 16 }))).toBeNull();
    expect(r.consumedLastEvent).toBe(false);
    // 누적이 버려졌으므로 다음 100px로는 아직 step에 닿지 않는다.
    expect(r.feed(wheel({ deltaX: 100, timeStamp: 32 }))).toBeNull();
    expect(r.feed(wheel({ deltaX: 100, timeStamp: 48 }))).toBe("next");
  });

  it("가로가 2배 이상이면 제스처다(대각선 허용)", () => {
    const r = new TabSwipeRecognizer({ stepPx: 60 });
    expect(r.feed(wheel({ deltaX: 70, deltaY: 30, timeStamp: 0 }))).toBe("next");
  });

  it("스와이프 중이어도 세로 이벤트는 절대 소비하지 않는다(터미널 스크롤 보존)", () => {
    const r = new TabSwipeRecognizer({ stepPx: 60 });
    expect(r.feed(wheel({ deltaX: 70, timeStamp: 0 }))).toBe("next");
    expect(r.feed(wheel({ deltaY: 80, timeStamp: 16 }))).toBeNull();
    expect(r.consumedLastEvent).toBe(false);
  });
});

describe("deltaMode", () => {
  it("줄 단위(1)는 16px로 환산한다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 120 });
    // 8줄 × 16px = 128px ≥ 120
    expect(r.feed(wheel({ deltaX: 8, deltaMode: 1, timeStamp: 0 }))).toBe("next");
  });

  it("줄 단위에서도 세로 우위 판정은 환산 뒤의 값으로 한다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 16 });
    expect(r.feed(wheel({ deltaX: 1, deltaY: 3, deltaMode: 1, timeStamp: 0 }))).toBeNull();
    expect(r.consumedLastEvent).toBe(false);
  });

  it("페이지 단위(2)는 무시한다(트랙패드 제스처가 아니다)", () => {
    const r = new TabSwipeRecognizer({ stepPx: 60 });
    expect(r.feed(wheel({ deltaX: 5, deltaMode: 2, timeStamp: 0 }))).toBeNull();
    expect(r.consumedLastEvent).toBe(false);
  });
});

describe("방향 반전 · 설정 반영", () => {
  it("reverse면 같은 제스처가 반대 탭을 고른다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 60, reverse: true });
    expect(r.feed(wheel({ deltaX: 70, timeStamp: 0 }))).toBe("prev");
    expect(r.feed(wheel({ deltaX: -70, timeStamp: 300 }))).toBe("next");
  });

  it("configure로 step을 갈아 끼울 수 있다(설정 변경 즉시 반영)", () => {
    const r = new TabSwipeRecognizer({ stepPx: 220 });
    expect(r.feed(wheel({ deltaX: 100, timeStamp: 0 }))).toBeNull();
    r.configure({ stepPx: 60 });
    expect(r.feed(wheel({ deltaX: 20, timeStamp: 16 }))).toBe("next");
  });
});

describe("소비 플래그 · reset", () => {
  it("데드존(8px) 아래에서는 소비하지 않는다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 120 });
    expect(TAB_SWIPE_DEAD_ZONE_PX).toBe(8);
    r.feed(wheel({ deltaX: 5, timeStamp: 0 }));
    expect(r.consumedLastEvent).toBe(false);
    r.feed(wheel({ deltaX: 5, timeStamp: 16 }));
    expect(r.consumedLastEvent).toBe(true); // 누적 10px — 제스처 진행 중
  });

  it("발동한 이벤트도 소비한다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 60 });
    r.feed(wheel({ deltaX: 70, timeStamp: 0 }));
    expect(r.consumedLastEvent).toBe(true);
  });

  it("reset은 진행 중인 제스처를 버린다", () => {
    const r = new TabSwipeRecognizer({ stepPx: 60 });
    expect(r.feed(wheel({ deltaX: 70, timeStamp: 0 }))).toBe("next");
    r.reset();
    expect(r.consumedLastEvent).toBe(false);
    // 리셋 뒤 첫 이벤트는 새 제스처다.
    expect(r.feed(wheel({ deltaX: 70, timeStamp: 16 }))).toBe("next");
  });
});
