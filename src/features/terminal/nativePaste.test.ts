import { describe, expect, it } from "vitest";
import { isNativePasteKey, terminalLeafIdOf } from "./nativePaste";
import { keyEvent, mapShortcut, type Platform, type ShortcutContext } from "./shortcuts";

const ctx = (platform: Platform): ShortcutContext => ({
  platform,
  imeComposing: false,
  modalOpen: false,
  hasSelection: false,
});

describe("isNativePasteKey", () => {
  it("macOS: only Cmd+V", () => {
    expect(isNativePasteKey(keyEvent({ code: "KeyV", key: "v", metaKey: true }), "darwin")).toBe(true);
    expect(isNativePasteKey(keyEvent({ code: "KeyV", key: "v", metaKey: true, shiftKey: true }), "darwin")).toBe(false);
    expect(isNativePasteKey(keyEvent({ code: "KeyV", key: "V", ctrlKey: true, shiftKey: true }), "darwin")).toBe(false);
    expect(isNativePasteKey(keyEvent({ code: "KeyV", key: "√", metaKey: true, altKey: true }), "darwin")).toBe(false);
    expect(isNativePasteKey(keyEvent({ code: "KeyC", key: "c", metaKey: true }), "darwin")).toBe(false);
  });

  it("Windows·Linux: Ctrl+Shift+V — plain Ctrl+V stays terminal input", () => {
    for (const platform of ["windows", "linux"] as const) {
      expect(isNativePasteKey(keyEvent({ code: "KeyV", key: "V", ctrlKey: true, shiftKey: true }), platform)).toBe(true);
      expect(isNativePasteKey(keyEvent({ code: "KeyV", key: "v", ctrlKey: true }), platform)).toBe(false);
      expect(isNativePasteKey(keyEvent({ code: "KeyV", key: "v", metaKey: true }), platform)).toBe(false);
    }
  });

  it("a Korean input source still counts — the key is not a Latin letter", () => {
    expect(isNativePasteKey(keyEvent({ code: "KeyV", key: "ㅍ", metaKey: true }), "darwin")).toBe(true);
  });

  it("a layout whose V position types another Latin letter (Dvorak) keeps the app path", () => {
    expect(isNativePasteKey(keyEvent({ code: "KeyV", key: "k", metaKey: true }), "darwin")).toBe(false);
    expect(isNativePasteKey(keyEvent({ code: "KeyV", key: "K", ctrlKey: true, shiftKey: true }), "windows")).toBe(false);
  });

  it("every native paste combo is the default paste shortcut", () => {
    const combos: Array<[Platform, Parameters<typeof keyEvent>[0]]> = [
      ["darwin", { code: "KeyV", key: "v", metaKey: true }],
      ["windows", { code: "KeyV", key: "V", ctrlKey: true, shiftKey: true }],
      ["linux", { code: "KeyV", key: "V", ctrlKey: true, shiftKey: true }],
    ];
    for (const [platform, patch] of combos) {
      const event = keyEvent(patch);
      expect(isNativePasteKey(event, platform)).toBe(true);
      expect(mapShortcut(event, ctx(platform))).toBe("paste");
    }
  });
});

describe("terminalLeafIdOf", () => {
  /** closest(selector)가 표에 있는 조상만 찾는 가짜 요소. */
  const element = (ancestors: Record<string, string | null>): EventTarget =>
    ({
      closest: (selector: string) =>
        selector in ancestors ? { getAttribute: (name: string) => (name === "data-leaf-id" ? ancestors[selector] : null) } : null,
    }) as unknown as EventTarget;

  it("returns the pane's leafId for a target inside xterm", () => {
    expect(terminalLeafIdOf(element({ ".xterm": null, "[data-leaf-id]": "leaf-7" }))).toBe("leaf-7");
  });

  it("returns null outside xterm, outside a pane, and for non-elements", () => {
    expect(terminalLeafIdOf(element({ "[data-leaf-id]": "leaf-7" }))).toBeNull(); // pane 안의 버튼·입력 칸
    expect(terminalLeafIdOf(element({ ".xterm": null }))).toBeNull();
    expect(terminalLeafIdOf(null)).toBeNull();
    expect(terminalLeafIdOf({} as EventTarget)).toBeNull();
  });
});
