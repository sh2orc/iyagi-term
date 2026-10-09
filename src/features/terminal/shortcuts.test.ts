import { describe, expect, it } from "vitest";
import {
  effectiveBinding,
  isReservedForTerminal,
  keyEvent,
  mapShortcut,
  OVERRIDABLE_ACTIONS,
  shortcutHint,
  validateShortcutOverrides,
  type ShortcutContext,
} from "./shortcuts";

const winCtx: ShortcutContext = {
  platform: "windows",
  imeComposing: false,
  modalOpen: false,
  hasSelection: false,
};

const macCtx: ShortcutContext = { ...winCtx, platform: "darwin" };
const linuxCtx: ShortcutContext = { ...winCtx, platform: "linux" };

describe("mapShortcut — splits and clipboard (windows/linux)", () => {
  it("maps Ctrl+Shift+D/E/W/P/C/V", () => {
    expect(mapShortcut(keyEvent({ code: "KeyD", ctrlKey: true, shiftKey: true }), winCtx)).toBe("split-row");
    expect(mapShortcut(keyEvent({ code: "KeyE", ctrlKey: true, shiftKey: true }), winCtx)).toBe("split-column");
    expect(mapShortcut(keyEvent({ code: "KeyW", ctrlKey: true, shiftKey: true }), winCtx)).toBe("close-pane");
    expect(mapShortcut(keyEvent({ code: "KeyP", ctrlKey: true, shiftKey: true }), winCtx)).toBe("palette");
    expect(mapShortcut(keyEvent({ code: "KeyC", ctrlKey: true, shiftKey: true }), winCtx)).toBe("copy");
    expect(mapShortcut(keyEvent({ code: "KeyV", ctrlKey: true, shiftKey: true }), winCtx)).toBe("paste");
  });

  it("passes plain Ctrl+D/C through to the terminal (EOF/interrupt)", () => {
    expect(mapShortcut(keyEvent({ code: "KeyD", ctrlKey: true }), winCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyC", ctrlKey: true }), winCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyD" }), winCtx)).toBeNull();
  });

  it("blocks pane actions during IME composition, modal, alt, and repeat", () => {
    const ev = keyEvent({ code: "KeyD", ctrlKey: true, shiftKey: true });
    expect(mapShortcut(ev, { ...winCtx, imeComposing: true })).toBeNull();
    expect(mapShortcut(ev, { ...winCtx, modalOpen: true })).toBeNull();
    expect(mapShortcut(keyEvent({ ...ev, altKey: true, repeat: true }), winCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ ...ev, repeat: true }), winCtx)).toBeNull();
  });
});

describe("mapShortcut — darwin", () => {
  it("maps Cmd+D / Cmd+Shift+D and Cmd+W/P/C/V", () => {
    expect(mapShortcut(keyEvent({ code: "KeyD", metaKey: true }), macCtx)).toBe("split-row");
    expect(mapShortcut(keyEvent({ code: "KeyD", metaKey: true, shiftKey: true }), macCtx)).toBe("split-column");
    expect(mapShortcut(keyEvent({ code: "KeyW", metaKey: true }), macCtx)).toBe("close-pane");
    expect(mapShortcut(keyEvent({ code: "KeyP", metaKey: true, shiftKey: true }), macCtx)).toBe("palette");
    expect(mapShortcut(keyEvent({ code: "KeyV", metaKey: true }), macCtx)).toBe("paste");
  });

  it("Cmd+C copies only with a selection, else no-op", () => {
    expect(mapShortcut(keyEvent({ code: "KeyC", metaKey: true }), { ...macCtx, hasSelection: true })).toBe("copy");
    expect(mapShortcut(keyEvent({ code: "KeyC", metaKey: true }), macCtx)).toBeNull();
  });
});

describe("mapShortcut — pane zoom (Ctrl/Cmd + =/-/0)", () => {
  it("maps Ctrl+= / Ctrl+- / Ctrl+0 on windows/linux", () => {
    expect(mapShortcut(keyEvent({ code: "Equal", key: "=", ctrlKey: true }), winCtx)).toBe("zoom-in");
    expect(mapShortcut(keyEvent({ code: "Minus", key: "-", ctrlKey: true }), winCtx)).toBe("zoom-out");
    expect(mapShortcut(keyEvent({ code: "Digit0", key: "0", ctrlKey: true }), winCtx)).toBe("zoom-reset");
  });

  it("Ctrl+Shift+= (the physical '+' chord) also zooms in", () => {
    expect(mapShortcut(keyEvent({ code: "Equal", key: "+", ctrlKey: true, shiftKey: true }), winCtx)).toBe("zoom-in");
  });

  it("maps numpad Add/Subtract and numpad 0", () => {
    expect(mapShortcut(keyEvent({ code: "NumpadAdd", key: "+", ctrlKey: true }), winCtx)).toBe("zoom-in");
    expect(mapShortcut(keyEvent({ code: "NumpadSubtract", key: "-", ctrlKey: true }), winCtx)).toBe("zoom-out");
    expect(mapShortcut(keyEvent({ code: "Numpad0", key: "0", ctrlKey: true }), winCtx)).toBe("zoom-reset");
  });

  it("zoom honors key repeat (hold-to-zoom)", () => {
    expect(mapShortcut(keyEvent({ code: "Equal", ctrlKey: true, repeat: true }), winCtx)).toBe("zoom-in");
    expect(mapShortcut(keyEvent({ code: "Minus", ctrlKey: true, repeat: true }), winCtx)).toBe("zoom-out");
  });

  it("maps Cmd+= / Cmd+- / Cmd+0 on darwin", () => {
    expect(mapShortcut(keyEvent({ code: "Equal", metaKey: true }), macCtx)).toBe("zoom-in");
    expect(mapShortcut(keyEvent({ code: "Minus", metaKey: true }), macCtx)).toBe("zoom-out");
    expect(mapShortcut(keyEvent({ code: "Digit0", metaKey: true }), macCtx)).toBe("zoom-reset");
  });

  it("passes through without the platform modifier (terminal input = - 0)", () => {
    expect(mapShortcut(keyEvent({ code: "Equal" }), winCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "Minus" }), winCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "Digit0", ctrlKey: true }), macCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "Equal", metaKey: true }), winCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "Equal", ctrlKey: true, metaKey: true }), winCtx)).toBeNull();
  });

  it("is blocked by modal/IME/alt like other app combos", () => {
    const ev = keyEvent({ code: "Equal", ctrlKey: true });
    expect(mapShortcut(ev, { ...winCtx, modalOpen: true })).toBeNull();
    expect(mapShortcut(ev, { ...winCtx, imeComposing: true })).toBeNull();
    expect(mapShortcut(keyEvent({ code: "Equal", ctrlKey: true, altKey: true }), winCtx)).toBeNull();
  });
});

describe("mapShortcut — 동시 입력 토글", () => {
  it("maps Cmd+Shift+B (darwin) and Ctrl+Shift+B (windows/linux)", () => {
    expect(mapShortcut(keyEvent({ code: "KeyB", metaKey: true, shiftKey: true }), macCtx)).toBe(
      "broadcast-toggle",
    );
    expect(mapShortcut(keyEvent({ code: "KeyB", ctrlKey: true, shiftKey: true }), winCtx)).toBe(
      "broadcast-toggle",
    );
  });

  it("maps Cmd+B / Ctrl+B to the workload panel and leaves plain B to the terminal", () => {
    expect(mapShortcut(keyEvent({ code: "KeyB", metaKey: true }), macCtx)).toBe("queue-toggle");
    expect(mapShortcut(keyEvent({ code: "KeyB", ctrlKey: true }), winCtx)).toBe("queue-toggle");
    expect(mapShortcut(keyEvent({ code: "KeyB" }), winCtx)).toBeNull();
  });

  it("is blocked by modal/IME/alt/repeat like other app combos", () => {
    const ev = keyEvent({ code: "KeyB", ctrlKey: true, shiftKey: true });
    expect(mapShortcut(ev, { ...winCtx, modalOpen: true })).toBeNull();
    expect(mapShortcut(ev, { ...winCtx, imeComposing: true })).toBeNull();
    // Alt는 터미널의 Meta로 남긴다 — iTerm2식 Cmd+Alt+I를 쓰지 않는 이유다.
    expect(mapShortcut(keyEvent({ ...ev, altKey: true }), winCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyB", metaKey: true, shiftKey: true, altKey: true }), macCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ ...ev, repeat: true }), winCtx)).toBeNull();
  });
});

describe("mapShortcut — 배치 편집 (layout-editor)", () => {
  it("maps Cmd+Shift+G (darwin) and Ctrl+Shift+G (windows/linux)", () => {
    expect(mapShortcut(keyEvent({ code: "KeyG", metaKey: true, shiftKey: true }), macCtx)).toBe(
      "layout-editor",
    );
    expect(mapShortcut(keyEvent({ code: "KeyG", ctrlKey: true, shiftKey: true }), winCtx)).toBe(
      "layout-editor",
    );
    expect(mapShortcut(keyEvent({ code: "KeyG", ctrlKey: true, shiftKey: true }), linuxCtx)).toBe(
      "layout-editor",
    );
  });

  it("Cmd+G(Shift 없음)와 맨 Ctrl+G(win/linux)는 그대로 터미널로 전달된다", () => {
    expect(mapShortcut(keyEvent({ code: "KeyG", metaKey: true }), macCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyG", ctrlKey: true }), winCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyG", ctrlKey: true }), linuxCtx)).toBeNull();
  });

  it("is blocked by modal/IME/alt/repeat like other app combos", () => {
    const ev = keyEvent({ code: "KeyG", ctrlKey: true, shiftKey: true });
    expect(mapShortcut(ev, { ...winCtx, modalOpen: true })).toBeNull();
    expect(mapShortcut(ev, { ...winCtx, imeComposing: true })).toBeNull();
    expect(mapShortcut(keyEvent({ ...ev, altKey: true }), winCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyG", metaKey: true, shiftKey: true, altKey: true }), macCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ ...ev, repeat: true }), winCtx)).toBeNull();
    expect(
      mapShortcut(keyEvent({ code: "KeyG", metaKey: true, shiftKey: true, repeat: true }), macCtx),
    ).toBeNull();
  });

  it('"pass" 재정의는 기본 조합을 터미널로 되돌려보낸다', () => {
    const overrides = { "layout-editor": "pass" as const };
    expect(
      mapShortcut(keyEvent({ code: "KeyG", metaKey: true, shiftKey: true }), { ...macCtx, overrides }),
    ).toBeNull();
    expect(
      mapShortcut(keyEvent({ code: "KeyG", ctrlKey: true, shiftKey: true }), { ...winCtx, overrides }),
    ).toBeNull();
  });

  it("사용자 재정의로 다른 조합(Ctrl+Shift+L)에 옮길 수 있다", () => {
    const overrides = { "layout-editor": { code: "KeyL", ctrl: true, meta: false, shift: true } };
    expect(
      mapShortcut(keyEvent({ code: "KeyL", ctrlKey: true, shiftKey: true }), { ...winCtx, overrides }),
    ).toBe("layout-editor");
  });

  it("validateShortcutOverrides: 다른 액션을 darwin 기본 Cmd+Shift+G에 배정하면 충돌", () => {
    const overrides = { copy: { code: "KeyG", ctrl: false, meta: true, shift: true } };
    expect(validateShortcutOverrides(overrides, "darwin")).toEqual([
      ["copy", "conflicts-default:layout-editor"],
    ]);
  });

  it("shortcutHint: darwin은 ⇧⌘G, windows는 Ctrl+Shift+G", () => {
    expect(shortcutHint("layout-editor", "darwin")).toBe("⇧⌘G");
    expect(shortcutHint("layout-editor", "windows")).toBe("Ctrl+Shift+G");
  });
});


describe("queue toggle guards", () => {
  it.each([macCtx, winCtx])("blocks repeated, composing, modal and Alt input ($platform)", (ctx) => {
    const ev = keyEvent({ code: "KeyB", metaKey: ctx.platform === "darwin", ctrlKey: ctx.platform !== "darwin" });
    expect(mapShortcut(ev, { ...ctx, modalOpen: true })).toBeNull();
    expect(mapShortcut(ev, { ...ctx, imeComposing: true })).toBeNull();
    expect(mapShortcut({ ...ev, repeat: true }, ctx)).toBeNull();
    expect(mapShortcut({ ...ev, altKey: true }, ctx)).toBeNull();
    expect(mapShortcut(ev, { ...ctx, overrides: { "queue-toggle": "pass" } })).toBeNull();
  });
});

describe("mapShortcut — 탭 순환(next-tab / prev-tab)", () => {
  it("darwin: Cmd+Shift+] 다음 탭, Cmd+Shift+[ 이전 탭", () => {
    expect(mapShortcut(keyEvent({ code: "BracketRight", key: "}", metaKey: true, shiftKey: true }), macCtx)).toBe(
      "next-tab",
    );
    expect(mapShortcut(keyEvent({ code: "BracketLeft", key: "{", metaKey: true, shiftKey: true }), macCtx)).toBe(
      "prev-tab",
    );
  });

  it("darwin: Shift 없는 Cmd+] 와 맨 ] 는 터미널 입력이다", () => {
    expect(mapShortcut(keyEvent({ code: "BracketRight", key: "]", metaKey: true }), macCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "BracketRight", key: "]" }), macCtx)).toBeNull();
  });

  it("windows/linux: 맨 Ctrl+PageDown/PageUp", () => {
    for (const ctx of [winCtx, linuxCtx]) {
      expect(mapShortcut(keyEvent({ code: "PageDown", key: "PageDown", ctrlKey: true }), ctx)).toBe("next-tab");
      expect(mapShortcut(keyEvent({ code: "PageUp", key: "PageUp", ctrlKey: true }), ctx)).toBe("prev-tab");
      // Ctrl+Shift+PageUp은 터미널 스크롤로 남겨 둔다.
      expect(
        mapShortcut(keyEvent({ code: "PageUp", key: "PageUp", ctrlKey: true, shiftKey: true }), ctx),
      ).toBeNull();
      // 맨 PageDown도 터미널 입력이다.
      expect(mapShortcut(keyEvent({ code: "PageDown", key: "PageDown" }), ctx)).toBeNull();
    }
  });

  it("다른 조합과 같은 가드를 받는다(모달·IME·Alt·반복)", () => {
    const mac = keyEvent({ code: "BracketRight", key: "}", metaKey: true, shiftKey: true });
    expect(mapShortcut(mac, { ...macCtx, modalOpen: true })).toBeNull();
    expect(mapShortcut(mac, { ...macCtx, imeComposing: true })).toBeNull();
    expect(mapShortcut({ ...mac, altKey: true }, macCtx)).toBeNull();
    expect(mapShortcut({ ...mac, repeat: true }, macCtx)).toBeNull();
    const win = keyEvent({ code: "PageDown", key: "PageDown", ctrlKey: true });
    expect(mapShortcut({ ...win, repeat: true }, winCtx)).toBeNull();
    expect(mapShortcut(win, { ...winCtx, modalOpen: true })).toBeNull();
  });

  it("재정의 대상이며 pass로 터미널에 돌려줄 수 있다", () => {
    expect(OVERRIDABLE_ACTIONS).toContain("next-tab");
    expect(OVERRIDABLE_ACTIONS).toContain("prev-tab");
    expect(
      mapShortcut(keyEvent({ code: "PageDown", key: "PageDown", ctrlKey: true }), {
        ...winCtx,
        overrides: { "next-tab": "pass" },
      }),
    ).toBeNull();
    expect(
      mapShortcut(keyEvent({ code: "KeyN", ctrlKey: true, shiftKey: true }), {
        ...winCtx,
        overrides: { "next-tab": { code: "KeyN", ctrl: true, meta: false, shift: true } },
      }),
    ).toBe("next-tab");
  });

  it("기본 바인딩·표시 문자열이 플랫폼을 따른다", () => {
    expect(effectiveBinding("next-tab", "darwin")).toEqual({
      code: "BracketRight",
      ctrl: false,
      meta: true,
      shift: true,
    });
    expect(effectiveBinding("prev-tab", "windows")).toEqual({
      code: "PageUp",
      ctrl: true,
      meta: false,
      shift: false,
    });
    expect(shortcutHint("next-tab", "darwin")).toBe("⇧⌘]");
    expect(shortcutHint("prev-tab", "darwin")).toBe("⇧⌘[");
    expect(shortcutHint("next-tab", "linux")).toBe("Ctrl+PgDn");
    expect(shortcutHint("prev-tab", "windows")).toBe("Ctrl+PgUp");
  });

  it("탭 순환 기본 조합을 다른 동작에 주려 하면 충돌로 막는다", () => {
    expect(
      validateShortcutOverrides({ copy: { code: "BracketRight", ctrl: false, meta: true, shift: true } }, "darwin"),
    ).toEqual([["copy", "conflicts-default:next-tab"]]);
  });
});

describe("mapShortcut — 새 AI 작업(new-mission)", () => {
  it("darwin Cmd+Shift+M, windows/linux Ctrl+Shift+M", () => {
    expect(mapShortcut(keyEvent({ code: "KeyM", key: "M", metaKey: true, shiftKey: true }), macCtx)).toBe("new-mission");
    for (const ctx of [winCtx, linuxCtx]) {
      expect(mapShortcut(keyEvent({ code: "KeyM", key: "M", ctrlKey: true, shiftKey: true }), ctx)).toBe("new-mission");
    }
  });

  it("맨 Cmd+M(시스템 최소화)·맨 Ctrl+M(CR)·맨 M은 앱이 가져가지 않는다", () => {
    expect(mapShortcut(keyEvent({ code: "KeyM", key: "m", metaKey: true }), macCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyM", key: "m", ctrlKey: true }), macCtx)).toBeNull();
    for (const ctx of [winCtx, linuxCtx]) {
      expect(mapShortcut(keyEvent({ code: "KeyM", key: "m", ctrlKey: true }), ctx)).toBeNull();
      expect(mapShortcut(keyEvent({ code: "KeyM", key: "m" }), ctx)).toBeNull();
    }
  });

  it("기본 조합이 다른 동작의 기본 조합과 겹치지 않고 터미널 예약 조합도 아니다", () => {
    for (const platform of ["darwin", "windows", "linux"] as const) {
      const binding = effectiveBinding("new-mission", platform);
      expect(binding, platform).not.toBeNull();
      expect(isReservedForTerminal(binding!), platform).toBe(false);
      // 기본 조합을 그대로 재정의로 넣어도 자기 자신 말고는 충돌 상대가 없다.
      expect(validateShortcutOverrides({ "new-mission": binding! }, platform), platform).toEqual([]);
      for (const other of OVERRIDABLE_ACTIONS) {
        if (other === "new-mission") continue;
        expect(effectiveBinding(other, platform), `${platform}:${other}`).not.toEqual(binding);
      }
    }
  });

  it("다른 조합과 같은 가드를 받고, 재정의·pass 대상이다", () => {
    const mac = keyEvent({ code: "KeyM", key: "M", metaKey: true, shiftKey: true });
    expect(mapShortcut(mac, { ...macCtx, modalOpen: true })).toBeNull();
    expect(mapShortcut(mac, { ...macCtx, imeComposing: true })).toBeNull();
    expect(mapShortcut({ ...mac, altKey: true }, macCtx)).toBeNull();
    expect(mapShortcut({ ...mac, repeat: true }, macCtx)).toBeNull();
    expect(OVERRIDABLE_ACTIONS).toContain("new-mission");
    expect(mapShortcut(mac, { ...macCtx, overrides: { "new-mission": "pass" } })).toBeNull();
  });

  it("다른 동작을 새 AI 작업 기본 조합에 주려 하면 충돌로 막는다", () => {
    expect(
      validateShortcutOverrides({ search: { code: "KeyM", ctrl: true, meta: false, shift: true } }, "windows"),
    ).toEqual([["search", "conflicts-default:new-mission"]]);
    expect(shortcutHint("new-mission", "darwin")).toBe("⇧⌘M");
    expect(shortcutHint("new-mission", "linux")).toBe("Ctrl+Shift+M");
  });
});

describe("mapShortcut — 일시정지 모두 재개(resume-all)", () => {
  it("darwin Cmd+Shift+R, windows/linux Ctrl+Shift+R", () => {
    expect(mapShortcut(keyEvent({ code: "KeyR", key: "R", metaKey: true, shiftKey: true }), macCtx)).toBe("resume-all");
    for (const ctx of [winCtx, linuxCtx]) {
      expect(mapShortcut(keyEvent({ code: "KeyR", key: "R", ctrlKey: true, shiftKey: true }), ctx)).toBe("resume-all");
    }
  });

  it("맨 Cmd+R·맨 Ctrl+R·맨 R은 앱이 가져가지 않는다", () => {
    expect(mapShortcut(keyEvent({ code: "KeyR", key: "r", metaKey: true }), macCtx)).toBeNull();
    expect(mapShortcut(keyEvent({ code: "KeyR", key: "r", ctrlKey: true }), macCtx)).toBeNull();
    for (const ctx of [winCtx, linuxCtx]) {
      expect(mapShortcut(keyEvent({ code: "KeyR", key: "r", ctrlKey: true }), ctx)).toBeNull();
      expect(mapShortcut(keyEvent({ code: "KeyR", key: "r" }), ctx)).toBeNull();
    }
  });

  it("기본 조합이 다른 동작의 기본 조합과 겹치지 않고 터미널 예약 조합도 아니다", () => {
    for (const platform of ["darwin", "windows", "linux"] as const) {
      const binding = effectiveBinding("resume-all", platform);
      expect(binding, platform).not.toBeNull();
      expect(isReservedForTerminal(binding!), platform).toBe(false);
      expect(validateShortcutOverrides({ "resume-all": binding! }, platform), platform).toEqual([]);
      for (const other of OVERRIDABLE_ACTIONS) {
        if (other === "resume-all") continue;
        expect(effectiveBinding(other, platform), `${platform}:${other}`).not.toEqual(binding);
      }
    }
  });

  it("다른 조합과 같은 가드를 받고, 재정의·pass 대상이다", () => {
    const mac = keyEvent({ code: "KeyR", key: "R", metaKey: true, shiftKey: true });
    expect(mapShortcut(mac, { ...macCtx, modalOpen: true })).toBeNull();
    expect(mapShortcut(mac, { ...macCtx, imeComposing: true })).toBeNull();
    expect(mapShortcut({ ...mac, altKey: true }, macCtx)).toBeNull();
    expect(mapShortcut({ ...mac, repeat: true }, macCtx)).toBeNull();
    expect(OVERRIDABLE_ACTIONS).toContain("resume-all");
    expect(mapShortcut(mac, { ...macCtx, overrides: { "resume-all": "pass" } })).toBeNull();
  });

  it("다른 동작을 일시정지 모두 재개 기본 조합에 주려 하면 충돌로 막는다", () => {
    expect(
      validateShortcutOverrides({ search: { code: "KeyR", ctrl: true, meta: false, shift: true } }, "windows"),
    ).toEqual([["search", "conflicts-default:resume-all"]]);
    expect(shortcutHint("resume-all", "darwin")).toBe("⇧⌘R");
    expect(shortcutHint("resume-all", "linux")).toBe("Ctrl+Shift+R");
  });
});
