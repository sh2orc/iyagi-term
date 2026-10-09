/**
 * 터미널 팔레트 — pane 배경을 테마와 다른 명암으로 덮어쓸 때 글자색을
 * 같이 뒤집어야 하는지 고르는 부분(readablePaletteFor).
 */

import { describe, expect, it } from "vitest";
import {
  DARK_THEME,
  LIGHT_THEME,
  colorBrightness,
  isDarkColor,
  readablePaletteFor,
  terminalTheme,
} from "./terminalPalette";

/** 밝은 테마에서 claude tint가 만드는 면(paneBackground.tintedSurface). */
const TINTED_DARK = "#160c08";

describe("colorBrightness / isDarkColor", () => {
  it("검정·흰색의 양 끝을 잡는다", () => {
    expect(colorBrightness("#000000")).toBe(0);
    expect(colorBrightness("#ffffff")).toBe(255);
    expect(isDarkColor("#000000")).toBe(true);
    expect(isDarkColor("#ffffff")).toBe(false);
  });
  it("형식이 어긋나면 판단하지 않는다(null)", () => {
    expect(colorBrightness("red")).toBeNull();
    expect(isDarkColor("#12345")).toBeNull();
  });
});

describe("readablePaletteFor", () => {
  it("밝은 테마 + 어두운 pane 배경이면 어두운 테마 글자색으로 바꾼다", () => {
    expect(readablePaletteFor(TINTED_DARK, LIGHT_THEME.foreground)).toBe(DARK_THEME);
  });
  it("어두운 테마 + 사용자가 고른 밝은 배경이면 밝은 테마 글자색으로 바꾼다", () => {
    expect(readablePaletteFor("#fff8e1", DARK_THEME.foreground)).toBe(LIGHT_THEME);
  });
  it("명암이 테마와 같은 방향이면 그대로 둔다(null)", () => {
    expect(readablePaletteFor(TINTED_DARK, DARK_THEME.foreground)).toBeNull();
    expect(readablePaletteFor("#fff8e1", LIGHT_THEME.foreground)).toBeNull();
  });
  it("형식이 어긋나면 그대로 둔다(null)", () => {
    expect(readablePaletteFor("nope", LIGHT_THEME.foreground)).toBeNull();
    expect(readablePaletteFor(TINTED_DARK, "nope")).toBeNull();
  });
});

describe("terminalTheme", () => {
  it("이름으로 두 팔레트를 고른다", () => {
    expect(terminalTheme("light")).toBe(LIGHT_THEME);
    expect(terminalTheme("dark")).toBe(DARK_THEME);
  });
});
