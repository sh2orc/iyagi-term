import { describe, expect, it } from "vitest";
import {
  DEFAULT_FONT_SIZE,
  MAX_FONT_SIZE,
  MIN_FONT_SIZE,
  clampFontSize,
  zoomFontSize,
} from "./zoom";

describe("clampFontSize", () => {
  it("keeps in-range sizes and rounds non-integers", () => {
    expect(clampFontSize(13)).toBe(13);
    expect(clampFontSize(13.4)).toBe(13);
    expect(clampFontSize(13.5)).toBe(14);
  });

  it("clamps to [MIN, MAX]", () => {
    expect(clampFontSize(1)).toBe(MIN_FONT_SIZE);
    expect(clampFontSize(999)).toBe(MAX_FONT_SIZE);
  });

  it("falls back to the default for non-finite input", () => {
    expect(clampFontSize(Number.NaN)).toBe(DEFAULT_FONT_SIZE);
    expect(clampFontSize(Number.POSITIVE_INFINITY)).toBe(DEFAULT_FONT_SIZE);
  });
});

describe("zoomFontSize", () => {
  it("steps by 1 in both directions", () => {
    expect(zoomFontSize(13, 1)).toBe(14);
    expect(zoomFontSize(13, -1)).toBe(12);
  });

  it("reset step returns the default regardless of current size", () => {
    expect(zoomFontSize(40, 0)).toBe(DEFAULT_FONT_SIZE);
    expect(zoomFontSize(6, 0)).toBe(DEFAULT_FONT_SIZE);
  });

  it("saturates at the limits (하한/상단 no-op)", () => {
    expect(zoomFontSize(MIN_FONT_SIZE, -1)).toBe(MIN_FONT_SIZE);
    expect(zoomFontSize(MAX_FONT_SIZE, 1)).toBe(MAX_FONT_SIZE);
  });

  it("recovers from out-of-range current values (결과는 항상 범위 안)", () => {
    expect(zoomFontSize(2, 1)).toBe(MIN_FONT_SIZE);
    expect(zoomFontSize(99, -1)).toBe(MAX_FONT_SIZE);
  });
});
