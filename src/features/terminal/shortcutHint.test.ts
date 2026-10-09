/**
 * 메뉴·툴팁용 단축키 표시: macOS 기호 표기, Windows/Linux 텍스트 표기,
 * 사용자 재정의 반영, "pass"(터미널로 돌려보냄)는 표시하지 않음.
 */

import { describe, expect, it } from "vitest";
import { shortcutHint } from "./shortcuts";

describe("shortcutHint", () => {
  it("darwin 기본표는 ⌃⇧⌘ 순서의 기호로 붙여 쓴다", () => {
    expect(shortcutHint("copy", "darwin")).toBe("⌘C");
    expect(shortcutHint("paste", "darwin")).toBe("⌘V");
    expect(shortcutHint("split-row", "darwin")).toBe("⌘D");
    expect(shortcutHint("split-column", "darwin")).toBe("⇧⌘D");
    expect(shortcutHint("broadcast-toggle", "darwin")).toBe("⇧⌘B");
    expect(shortcutHint("zoom-in", "darwin")).toBe("⌘=");
    expect(shortcutHint("zoom-out", "darwin")).toBe("⌘-");
    expect(shortcutHint("zoom-reset", "darwin")).toBe("⌘0");
    expect(shortcutHint("close-pane", "darwin")).toBe("⌘W");
  });

  it("windows/linux 기본표는 Ctrl+Shift 텍스트로 적는다", () => {
    expect(shortcutHint("split-row", "windows")).toBe("Ctrl+Shift+D");
    expect(shortcutHint("split-column", "windows")).toBe("Ctrl+Shift+E");
    expect(shortcutHint("paste", "windows")).toBe("Ctrl+Shift+V");
    expect(shortcutHint("search", "linux")).toBe("Ctrl+Shift+F");
    expect(shortcutHint("zoom-reset", "linux")).toBe("Ctrl+0");
    expect(shortcutHint("zoom-in", "windows")).toBe("Ctrl+=");
  });

  it("사용자 재정의가 있으면 기본 대신 그 조합을 보인다", () => {
    expect(
      shortcutHint("search", "darwin", { search: { code: "KeyG", ctrl: true, meta: false, shift: true } }),
    ).toBe("⌃⇧G");
    expect(
      shortcutHint("split-row", "windows", { "split-row": { code: "Digit1", ctrl: true, meta: false, shift: true } }),
    ).toBe("Ctrl+Shift+1");
    expect(
      shortcutHint("palette", "linux", { palette: { code: "KeyK", ctrl: false, meta: true, shift: true } }),
    ).toBe("Super+Shift+K");
    expect(
      shortcutHint("palette", "windows", { palette: { code: "KeyK", ctrl: true, meta: true, shift: false } }),
    ).toBe("Ctrl+Win+K");
  });

  it("터미널로 돌려보낸(pass) 액션은 표시하지 않는다", () => {
    expect(shortcutHint("copy", "darwin", { copy: "pass" })).toBeNull();
    expect(shortcutHint("zoom-in", "windows", { "zoom-in": "pass" })).toBeNull();
  });

  it("짧은 이름이 없는 키는 code 그대로 적는다", () => {
    expect(
      shortcutHint("search", "windows", { search: { code: "F3", ctrl: true, meta: false, shift: true } }),
    ).toBe("Ctrl+Shift+F3");
  });
});
