import { describe, expect, it } from "vitest";
import { BADGE_SHOW_AFTER_MS, STALE_OLD_MS, STALE_WARN_MS, elapsedPhrase, showsLastOutput, staleClass } from "./lastOutput";

describe("elapsedPhrase — 마지막 출력 경과 문구", () => {
  it("59초까지는 초, 60초부터 분을 센다", () => {
    expect(elapsedPhrase(59_000)).toEqual({ key: "terminal.pane.lastOutput.seconds", params: { n: 59 } });
    expect(elapsedPhrase(60_000)).toEqual({ key: "terminal.pane.lastOutput.minutes", params: { n: 1 } });
    expect(elapsedPhrase(3599_000)).toEqual({ key: "terminal.pane.lastOutput.minutes", params: { n: 59 } });
  });

  it("시간 단위에서는 남은 분이 있을 때만 함께 적는다", () => {
    expect(elapsedPhrase(3600_000)).toEqual({ key: "terminal.pane.lastOutput.hours", params: { n: 1 } });
    expect(elapsedPhrase(3660_000)).toEqual({
      key: "terminal.pane.lastOutput.hoursMinutes",
      params: { h: 1, m: 1 },
    });
    expect(elapsedPhrase((23 * 60 + 59) * 60_000)).toEqual({
      key: "terminal.pane.lastOutput.hoursMinutes",
      params: { h: 23, m: 59 },
    });
  });

  it("24시간부터는 일로 센다", () => {
    expect(elapsedPhrase(24 * 3600_000)).toEqual({ key: "terminal.pane.lastOutput.days", params: { n: 1 } });
    expect(elapsedPhrase(3 * 24 * 3600_000 + 7200_000)).toEqual({
      key: "terminal.pane.lastOutput.days",
      params: { n: 3 },
    });
  });

  it("음수(시계 오차)는 0으로 자른다", () => {
    expect(elapsedPhrase(-5_000)).toEqual({ key: "terminal.pane.lastOutput.seconds", params: { n: 0 } });
  });
});

describe("showsLastOutput — 배지 표시 문턱", () => {
  it("10초 미만은 숨기고 10초부터 보인다", () => {
    expect(BADGE_SHOW_AFTER_MS).toBe(10_000);
    expect(showsLastOutput(0)).toBe(false);
    expect(showsLastOutput(BADGE_SHOW_AFTER_MS - 1)).toBe(false);
    expect(showsLastOutput(BADGE_SHOW_AFTER_MS)).toBe(true);
  });
});

describe("staleClass — 경과 강조 단계", () => {
  it("5분 미만은 평소, 5분·30분을 지나면 단계적으로 강조한다", () => {
    expect(staleClass(4 * 60_000)).toBe("");
    expect(staleClass(STALE_WARN_MS)).toBe(" stale");
    expect(staleClass(29 * 60_000)).toBe(" stale");
    expect(staleClass(STALE_OLD_MS)).toBe(" stale-old");
  });
});
