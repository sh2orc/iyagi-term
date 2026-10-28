import { describe, expect, it } from "vitest";
import { revealTabScroll } from "./tabReveal";

describe("tabReveal", () => {
  const bar = (scrollLeft: number, clientWidth = 300) => ({ scrollLeft, clientWidth });
  const tab = (offsetLeft: number, offsetWidth = 100) => ({ offsetLeft, offsetWidth });

  it("보이는 탭은 scrollLeft를 바꾸지 않는다(null)", () => {
    expect(revealTabScroll(bar(0), tab(50))).toBeNull();
    expect(revealTabScroll(bar(100), tab(100))).toBeNull(); // 왼쪽이 창 시작에 정확히 닿음
  });

  it("오른쪽으로 밀려 나간 탭은 오른쪽 끝이 창 안에 들어오게 만다", () => {
    // 창 [0,300], 탭 [280,380] → 목표 scrollLeft = 380-300+6 = 86
    expect(revealTabScroll(bar(0), tab(280))).toBe(86);
  });

  it("왼쪽으로 밀려 나간 탭은 왼쪽 끝에 여백을 두고 맞춘다", () => {
    // 창 [200,500], 탭 [150,250] → 목표 150-6 = 144
    expect(revealTabScroll(bar(200), tab(150))).toBe(144);
  });

  it("왼쪽 정렬이 음수가 되면 0으로 고정한다", () => {
    // 창 [10,310], 탭 [8,108] → 8-6 = 2 (0 아님)
    expect(revealTabScroll(bar(10), tab(8, 100))).toBe(2);
    // 창 [5,305], 탭 [0,100] → -6 → 0
    expect(revealTabScroll(bar(5), tab(0, 100))).toBe(0);
  });

  it("탭이 창보다 넓으면 오른쪽 끝을 맞춘다(왼쪽 일부는 계속 가려진다)", () => {
    // 창 [0,300], 탭 [0,400] → 400-300+6 = 106
    expect(revealTabScroll(bar(0), tab(0, 400))).toBe(106);
  });
});
