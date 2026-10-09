import { describe, expect, it } from "vitest";
import {
  bindingLabel,
  keyEvent,
  mapShortcut,
  validateShortcutOverrides,
  type ShortcutContext,
} from "../src/features/terminal/shortcuts";

function ctx(patch: Partial<ShortcutContext> = {}): ShortcutContext {
  return {
    platform: "windows",
    imeComposing: false,
    modalOpen: false,
    hasSelection: false,
    ...patch,
  };
}

describe("platform table — 04-ui.md §3", () => {
  it("macOS: Cmd+D splits row, Cmd+Shift+D splits column", () => {
    expect(mapShortcut(keyEvent({ code: "KeyD", key: "d", metaKey: true }), ctx({ platform: "darwin" }))).toBe("split-row");
    expect(
      mapShortcut(keyEvent({ code: "KeyD", key: "D", metaKey: true, shiftKey: true }), ctx({ platform: "darwin" })),
    ).toBe("split-column");
  });

  it("Windows/Linux: Ctrl+Shift+D splits row, Ctrl+Shift+E splits column", () => {
    for (const platform of ["windows", "linux"] as const) {
      expect(
        mapShortcut(keyEvent({ code: "KeyD", key: "D", ctrlKey: true, shiftKey: true }), ctx({ platform })),
      ).toBe("split-row");
      expect(
        mapShortcut(keyEvent({ code: "KeyE", key: "E", ctrlKey: true, shiftKey: true }), ctx({ platform })),
      ).toBe("split-column");
    }
  });

  it("pane close: Cmd+W (mac) / Ctrl+Shift+W (win)", () => {
    expect(mapShortcut(keyEvent({ code: "KeyW", key: "w", metaKey: true }), ctx({ platform: "darwin" }))).toBe("close-pane");
    expect(mapShortcut(keyEvent({ code: "KeyW", key: "W", ctrlKey: true, shiftKey: true }), ctx())).toBe("close-pane");
  });

  it("palette: Cmd+Shift+P / Ctrl+Shift+P", () => {
    expect(
      mapShortcut(keyEvent({ code: "KeyP", key: "P", metaKey: true, shiftKey: true }), ctx({ platform: "darwin" })),
    ).toBe("palette");
    expect(mapShortcut(keyEvent({ code: "KeyP", key: "P", ctrlKey: true, shiftKey: true }), ctx())).toBe("palette");
  });

  it("copy: mac Cmd+C requires selection(no-selection no-op), win Ctrl+Shift+C always", () => {
    expect(mapShortcut(keyEvent({ code: "KeyC", key: "c", metaKey: true }), ctx({ platform: "darwin", hasSelection: true }))).toBe("copy");
    expect(mapShortcut(keyEvent({ code: "KeyC", key: "c", metaKey: true }), ctx({ platform: "darwin", hasSelection: false }))).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyC", key: "C", ctrlKey: true, shiftKey: true }), ctx({ hasSelection: false }))).toBe("copy");
  });

  it("search: Cmd+F (mac) / Ctrl+Shift+F (win) — 맨 Ctrl+F는 터미널 통과(W1-9)", () => {
    expect(mapShortcut(keyEvent({ code: "KeyF", key: "f", metaKey: true }), ctx({ platform: "darwin" }))).toBe("search");
    expect(mapShortcut(keyEvent({ code: "KeyF", key: "F", ctrlKey: true, shiftKey: true }), ctx())).toBe("search");
    // readline forward-char: 맨 Ctrl+F는 앱이 가로채지 않는다.
    expect(mapShortcut(keyEvent({ code: "KeyF", key: "f", ctrlKey: true }), ctx())).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyF", key: "F", ctrlKey: true, shiftKey: true }), ctx({ platform: "darwin" }))).toBeNull();
  });

  it("paste: Cmd+V / Ctrl+Shift+V", () => {
    expect(mapShortcut(keyEvent({ code: "KeyV", key: "v", metaKey: true }), ctx({ platform: "darwin" }))).toBe("paste");
    expect(mapShortcut(keyEvent({ code: "KeyV", key: "V", ctrlKey: true, shiftKey: true }), ctx())).toBe("paste");
  });
});

describe("terminal passthrough — U06", () => {
  it("Windows/Linux Ctrl+D is NOT an app action (0x04 EOF goes to the PTY)", () => {
    for (const platform of ["windows", "linux"] as const) {
      expect(mapShortcut(keyEvent({ code: "KeyD", key: "d", ctrlKey: true }), ctx({ platform }))).toBeNull();
    }
  });

  it("Ctrl+C is ALWAYS terminal input, regardless of selection", () => {
    for (const platform of ["darwin", "windows", "linux"] as const) {
      expect(mapShortcut(keyEvent({ code: "KeyC", key: "c", ctrlKey: true }), ctx({ platform, hasSelection: true }))).toBeNull();
    }
  });

  it("mac Ctrl+D / Ctrl+W are not intercepted either", () => {
    expect(mapShortcut(keyEvent({ code: "KeyD", key: "d", ctrlKey: true }), ctx({ platform: "darwin" }))).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyW", key: "w", ctrlKey: true }), ctx({ platform: "darwin" }))).toBeNull();
  });

  it("plain typing never matches", () => {
    expect(mapShortcut(keyEvent({ code: "KeyD", key: "d" }), ctx())).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyP", key: "p", shiftKey: true }), ctx())).toBeNull();
  });
});

describe("guards — U07 IME / repeat / modal", () => {
  it("IME composition blocks pane creation/focus shortcuts", () => {
    expect(
      mapShortcut(keyEvent({ code: "KeyD", key: "d", metaKey: true }), ctx({ platform: "darwin", imeComposing: true })),
    ).toBeNull();
    expect(
      mapShortcut(keyEvent({ code: "KeyD", key: "D", ctrlKey: true, shiftKey: true }), ctx({ imeComposing: true })),
    ).toBeNull();
    expect(
      mapShortcut(keyEvent({ code: "KeyW", key: "W", ctrlKey: true, shiftKey: true }), ctx({ imeComposing: true })),
    ).toBeNull();
  });

  it("key repeat is ignored", () => {
    expect(mapShortcut(keyEvent({ code: "KeyD", key: "d", metaKey: true, repeat: true }), ctx({ platform: "darwin" }))).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyD", key: "D", ctrlKey: true, shiftKey: true, repeat: true }), ctx())).toBeNull();
  });

  it("open modal blocks app shortcuts", () => {
    expect(
      mapShortcut(keyEvent({ code: "KeyD", key: "D", ctrlKey: true, shiftKey: true }), ctx({ modalOpen: true })),
    ).toBeNull();
    expect(
      mapShortcut(keyEvent({ code: "KeyP", key: "P", ctrlKey: true, shiftKey: true }), ctx({ modalOpen: true })),
    ).toBeNull();
  });
});

describe("단축키 재정의(W3-2) — 조건 4개", () => {
  const binding = (code: string, mods: Partial<{ ctrl: boolean; meta: boolean; shift: boolean }> = {}) => ({
    code,
    ctrl: mods.ctrl ?? false,
    meta: mods.meta ?? false,
    shift: mods.shift ?? false,
  });

  it("① 바인딩 테이블만 재정의된다 — IME·모달 가드는 우회 불가", () => {
    const overrides = { "split-row": binding("KeyT", { ctrl: true, shift: true }) };
    expect(
      mapShortcut(keyEvent({ code: "KeyT", key: "T", ctrlKey: true, shiftKey: true }), ctx({ overrides })),
    ).toBe("split-row");
    // 같은 조합도 IME 조합 중/모달 열림에는 발동하지 않는다.
    expect(
      mapShortcut(keyEvent({ code: "KeyT", key: "T", ctrlKey: true, shiftKey: true }), ctx({ overrides, imeComposing: true })),
    ).toBeNull();
    expect(
      mapShortcut(keyEvent({ code: "KeyT", key: "T", ctrlKey: true, shiftKey: true }), ctx({ overrides, modalOpen: true })),
    ).toBeNull();
  });

  it("② 터미널 통과키는 예약 — 검증이 거부한다(맨 Ctrl·맨 키)", () => {
    expect(validateShortcutOverrides({ copy: binding("KeyX", { ctrl: true }) }, "windows")).toEqual([
      ["copy", "reserved"],
    ]);
    expect(validateShortcutOverrides({ paste: binding("KeyX") }, "darwin")).toEqual([
      ["paste", "reserved"],
    ]);
    expect(validateShortcutOverrides({ copy: binding("KeyX", { ctrl: true, shift: true }) }, "windows")).toEqual([]);
  });

  it("③ 충돌 검사 — 재정의 간·기본표와 같은 조합 거부", () => {
    const both = {
      "split-row": binding("KeyT", { ctrl: true, shift: true }),
      "close-pane": binding("KeyT", { ctrl: true, shift: true }),
    } as const;
    const errors = validateShortcutOverrides(both, "windows");
    expect(errors).toHaveLength(1);
    expect(errors[0][1]).toContain("conflicts");
    // 기본표(Ctrl+Shift+D = split-row)를 다른 액션에 주는 것도 거부.
    expect(
      validateShortcutOverrides({ "close-pane": binding("KeyD", { ctrl: true, shift: true }) }, "windows"),
    ).toHaveLength(1);
  });

  it("④ pass 재정의는 기본 조합을 터미널로 되돌려보낸다", () => {
    const overrides = { paste: "pass" as const };
    // 기본: Ctrl+Shift+V → paste.
    expect(mapShortcut(keyEvent({ code: "KeyV", key: "V", ctrlKey: true, shiftKey: true }), ctx())).toBe("paste");
    expect(
      mapShortcut(keyEvent({ code: "KeyV", key: "V", ctrlKey: true, shiftKey: true }), ctx({ overrides })),
    ).toBeNull();
  });
});

describe("Windows의 meta(Win 키) — W3 #9", () => {
  const binding = (code: string, mods: Partial<{ ctrl: boolean; meta: boolean; shift: boolean }> = {}) => ({
    code,
    ctrl: mods.ctrl ?? false,
    meta: mods.meta ?? false,
    shift: mods.shift ?? false,
  });

  it("Win+글쇠 재정의는 Windows에서 거절된다(OS가 먼저 가져간다); macOS의 Cmd는 그대로", () => {
    expect(validateShortcutOverrides({ copy: binding("KeyX", { meta: true, shift: true }) }, "windows")).toEqual([
      ["copy", "reserved"],
    ]);
    expect(validateShortcutOverrides({ copy: binding("KeyX", { meta: true, shift: true }) }, "darwin")).toEqual([]);
    expect(validateShortcutOverrides({ copy: binding("KeyX", { ctrl: true, shift: true }) }, "windows")).toEqual([]);
    // Linux: Super+글쇠는 GNOME/KDE가 가져간다.
    expect(validateShortcutOverrides({ copy: binding("KeyX", { meta: true, shift: true }) }, "linux")).toEqual([
      ["copy", "reserved"],
    ]);
    expect(validateShortcutOverrides({ copy: binding("KeyX", { ctrl: true, shift: true }) }, "linux")).toEqual([]);
  });

  it("bindingLabel은 meta를 플랫폼 이름으로 부른다", () => {
    const b = binding("KeyX", { meta: true, shift: true });
    expect(bindingLabel(b, "darwin")).toBe("Cmd+Shift+X");
    expect(bindingLabel(b, "windows")).toBe("Win+Shift+X");
    expect(bindingLabel(b, "linux")).toBe("Super+Shift+X");
    expect(bindingLabel(binding("Digit0", { ctrl: true }), "windows")).toBe("Ctrl+0");
  });
});
