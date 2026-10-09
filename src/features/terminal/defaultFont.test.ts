import { describe, expect, it } from "vitest";
import {
  BASE_LINE_HEIGHT,
  isFontRendered,
  lineHeightForCharHeight,
  KOREAN_MONO_CANDIDATES,
  LATIN_FALLBACK_STACK,
  pickDefaultFontFamily,
} from "./defaultFont";

describe("pickDefaultFontFamily", () => {
  it("설치된 첫 한국어 코딩 글꼴을 라틴 폴백 앞에 올린다", () => {
    const stack = pickDefaultFontFamily((family) => family === "D2Coding");
    expect(stack).toBe(`"D2Coding", ${LATIN_FALLBACK_STACK}`);
  });

  it("첫 후보가 없으면 다음 후보를 쓴다", () => {
    const stack = pickDefaultFontFamily((family) => family === "Nanum Gothic Coding");
    expect(stack).toBe(`"Nanum Gothic Coding", ${LATIN_FALLBACK_STACK}`);
  });

  it("후보가 하나도 없으면 라틴 폴백 스택 그대로다", () => {
    expect(pickDefaultFontFamily(() => false)).toBe(LATIN_FALLBACK_STACK);
  });

  it("checker 예외는 후보 없음으로 취급한다", () => {
    expect(pickDefaultFontFamily(() => {
      throw new Error("FontFaceSet unavailable");
    })).toBe(LATIN_FALLBACK_STACK);
  });

  it("라틴 폴백 스택은 세 플랫폼의 관례 글꼴을 모두 담는다", () => {
    for (const face of ["Consolas", "Menlo", "'DejaVu Sans Mono'", "'Ubuntu Mono'", "'Noto Sans Mono'"]) {
      expect(LATIN_FALLBACK_STACK).toContain(face);
    }
    expect(LATIN_FALLBACK_STACK.endsWith("monospace")).toBe(true);
  });

  it("후보 목록은 코딩 폰트 우선 순서를 유지한다", () => {
    expect(KOREAN_MONO_CANDIDATES[0]).toBe("D2Coding");
    expect(KOREAN_MONO_CANDIDATES).toContain("Nanum Gothic Coding");
  });
});

describe("isFontRendered", () => {
  // 가짜 측정기: 설치된 글꼴이 스택 앞에 있으면 그 폭, 아니면 generic 폭.
  const measurer = (installed: Record<string, number>) => (font: string): number => {
    const named = /"([^"]+)"/.exec(font)?.[1];
    if (named && installed[named] !== undefined) return installed[named];
    return font.endsWith("serif") ? 90 : 100;
  };

  it("없는 글꼴은 폴백과 폭이 같아 없는 것으로 본다", () => {
    // WKWebView의 document.fonts.check는 이 경우에도 true를 돌려준다.
    expect(isFontRendered("D2Coding", measurer({}))).toBe(false);
  });

  it("그려지는 글꼴은 폴백과 폭이 달라 있는 것으로 본다", () => {
    expect(isFontRendered("Nanum Gothic Coding", measurer({ "Nanum Gothic Coding": 104 }))).toBe(true);
  });

  it("한 generic과 우연히 폭이 같아도 다른 generic과 비교해 찾는다", () => {
    expect(isFontRendered("D2Coding", measurer({ D2Coding: 100 }))).toBe(true);
  });

  it("측정할 수 없으면 없는 것으로 본다", () => {
    expect(isFontRendered("D2Coding", () => null)).toBe(false);
  });

  it("없는 첫 후보를 건너뛰고 내장 글꼴을 고른다", () => {
    const measure = measurer({ "Nanum Gothic Coding": 104 });
    expect(pickDefaultFontFamily((family) => isFontRendered(family, measure)))
      .toBe(`"Nanum Gothic Coding", ${LATIN_FALLBACK_STACK}`);
  });
});

describe("lineHeightForCharHeight", () => {
  it("Menlo처럼 글자가 높은 글꼴은 기본 1.4를 지킨다", () => {
    expect(lineHeightForCharHeight(15 / 13)).toBe(BASE_LINE_HEIGHT);
    expect(lineHeightForCharHeight(1.25)).toBe(BASE_LINE_HEIGHT);
  });

  it("글자 높이가 1.0em인 Nanum Gothic Coding은 Menlo와 같은 행 높이로 늘린다", () => {
    const lineHeight = lineHeightForCharHeight(1);
    expect(lineHeight).toBe(1.62);
    // 13px에서 행 높이: Menlo 15 × 1.4 = 21px와 같다(반올림 오차 안).
    expect(Math.abs(13 * lineHeight - 15 * BASE_LINE_HEIGHT)).toBeLessThan(0.1);
  });

  it("잴 수 없으면 기본값", () => {
    expect(lineHeightForCharHeight(null)).toBe(BASE_LINE_HEIGHT);
    expect(lineHeightForCharHeight(0)).toBe(BASE_LINE_HEIGHT);
    expect(lineHeightForCharHeight(Number.NaN)).toBe(BASE_LINE_HEIGHT);
  });
});
