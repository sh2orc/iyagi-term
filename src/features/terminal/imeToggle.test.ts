import { describe, expect, it, vi } from "vitest";

import type { IpcAdapter } from "../bridge/ipc";
import {
  createImeToggle,
  isImeToggleChord,
  showImeBalloon,
  type ImeHudState,
  type ImeState,
} from "./imeToggle";

function keydown(overrides: Partial<KeyboardEvent> = {}): KeyboardEvent {
  const base = {
    type: "keydown",
    code: "Space",
    shiftKey: true,
    ctrlKey: false,
    altKey: false,
    metaKey: false,
    isComposing: false,
    repeat: false,
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
  };
  return { ...base, ...overrides } as unknown as KeyboardEvent;
}

function fakeIpc(handlers: Record<string, () => Promise<unknown>>) {
  const calls: string[] = [];
  const ipc: IpcAdapter = {
    invoke<T>(command: string): Promise<T> {
      calls.push(command);
      const handler = handlers[command];
      if (!handler) return Promise.reject(new Error(`no handler for ${command}`));
      return handler() as Promise<T>;
    },
    channel(): unknown {
      return {};
    },
  };
  return { ipc, calls };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("isImeToggleChord", () => {
  it("matches exactly Shift+Space on keydown", () => {
    expect(isImeToggleChord(keydown())).toBe(true);
    expect(isImeToggleChord(keydown({ type: "keyup" }))).toBe(false);
    expect(isImeToggleChord(keydown({ code: "Enter" }))).toBe(false);
    expect(isImeToggleChord(keydown({ shiftKey: false }))).toBe(false);
    expect(isImeToggleChord(keydown({ ctrlKey: true }))).toBe(false);
    expect(isImeToggleChord(keydown({ altKey: true }))).toBe(false);
    expect(isImeToggleChord(keydown({ metaKey: true }))).toBe(false);
    expect(isImeToggleChord(keydown({ isComposing: true }))).toBe(false);
    expect(isImeToggleChord(keydown({ repeat: true }))).toBe(false);
  });
});

describe("createImeToggle", () => {
  const korean: ImeState = {
    available: true,
    native: true,
    family: "ko",
    sourceId: "com.apple.inputmethod.Korean.2SetKorean",
    reason: null,
  };
  const chinese: ImeState = {
    available: true,
    native: true,
    family: "zh",
    sourceId: "com.apple.inputmethod.SCIM.ITABC",
    reason: null,
  };

  it("is off without Tauri IPC (browser preview) and lets the space through", () => {
    const hud = vi.fn();
    const toggle = createImeToggle({ ipc: null, getMode: () => "on", showHud: hud });
    const event = keydown();
    expect(toggle.enabled()).toBe(false);
    expect(toggle.handleKeydown(event, {})).toBe(false);
    expect(event.preventDefault).not.toHaveBeenCalled();
    expect(hud).not.toHaveBeenCalled();
  });

  it("auto: consumes the chord only once ime_state reported availability", async () => {
    const { ipc, calls } = fakeIpc({
      ime_state: async () => ({ ...korean }),
      ime_toggle: async () => ({ ...korean, native: false }),
    });
    const hud = vi.fn();
    const toggle = createImeToggle({ ipc, getMode: () => "auto", showHud: hud });
    // 아직 모른다: 첫 키는 통과시키되 조회를 건다.
    expect(toggle.handleKeydown(keydown(), {})).toBe(false);
    await flush();
    expect(calls).toEqual(["ime_state"]);
    expect(toggle.availability()).toBe(true);
    const event = keydown();
    expect(toggle.handleKeydown(event, { id: "term" })).toBe(true);
    expect(event.preventDefault).toHaveBeenCalled();
    await flush();
    expect(calls).toEqual(["ime_state", "ime_toggle"]);
    expect(hud).toHaveBeenCalledWith({ id: "term" }, { ...korean, native: false });
  });

  it("auto: stays off when the OS has no CJK input source", async () => {
    const { ipc } = fakeIpc({
      ime_state: async () => ({ available: false, native: null, family: null, sourceId: null, reason: "none" }),
    });
    const toggle = createImeToggle({ ipc, getMode: () => "auto", showHud: vi.fn() });
    await toggle.refresh();
    expect(toggle.enabled()).toBe(false);
    expect(toggle.handleKeydown(keydown(), {})).toBe(false);
  });

  it("on: consumes the chord, is single-flight, and reports failures in the HUD", async () => {
    let resolveToggle: (state: ImeState) => void = () => undefined;
    const { ipc, calls } = fakeIpc({
      ime_toggle: () => new Promise<ImeState>((resolve) => (resolveToggle = resolve)),
    });
    const hud = vi.fn<(terminal: unknown, state: ImeHudState) => void>();
    const toggle = createImeToggle({ ipc, getMode: () => "on", showHud: hud });
    expect(toggle.handleKeydown(keydown(), {})).toBe(true);
    expect(toggle.handleKeydown(keydown(), {})).toBe(true); // 연타는 삼키되 두 번 부르지 않는다
    expect(calls).toEqual(["ime_toggle"]);
    resolveToggle({ ...korean, native: true });
    await flush();
    expect(hud).toHaveBeenLastCalledWith({}, { ...korean, native: true });

    const failing = fakeIpc({ ime_toggle: async () => Promise.reject(new Error("boom")) });
    const hud2 = vi.fn<(terminal: unknown, state: ImeHudState) => void>();
    const toggle2 = createImeToggle({ ipc: failing.ipc, getMode: () => "on", showHud: hud2 });
    expect(toggle2.handleKeydown(keydown(), {})).toBe(true);
    await flush();
    expect(hud2).toHaveBeenCalledWith({}, { error: "boom" });
  });

  it("off: never consumes the chord", () => {
    const { ipc, calls } = fakeIpc({ ime_toggle: async () => korean });
    const toggle = createImeToggle({ ipc, getMode: () => "off", showHud: vi.fn() });
    expect(toggle.handleKeydown(keydown(), {})).toBe(false);
    expect(calls).toEqual([]);
  });

  it("onState gets the same result as the HUD, and a throwing callback never flips it to an error", async () => {
    const { ipc } = fakeIpc({ ime_toggle: async () => ({ ...chinese, native: false }) });
    const hud = vi.fn<(terminal: unknown, state: ImeHudState) => void>();
    const onState = vi.fn<(state: ImeHudState) => void>(() => {
      throw new Error("caller bug");
    });
    const toggle = createImeToggle({ ipc, getMode: () => "on", showHud: hud });
    expect(toggle.handleKeydown(keydown(), { id: "term" }, onState)).toBe(true);
    await flush();
    expect(onState).toHaveBeenCalledWith({ ...chinese, native: false });
    expect(hud).toHaveBeenCalledTimes(1);
    expect(hud).toHaveBeenCalledWith({ id: "term" }, { ...chinese, native: false });

    const failing = fakeIpc({ ime_toggle: async () => Promise.reject(new Error("boom")) });
    const onFailure = vi.fn<(state: ImeHudState) => void>();
    const toggle2 = createImeToggle({ ipc: failing.ipc, getMode: () => "on", showHud: vi.fn() });
    expect(toggle2.handleKeydown(keydown(), {}, onFailure)).toBe(true);
    await flush();
    expect(onFailure).toHaveBeenCalledWith({ error: "boom" });
  });

  it("tracks the last-known family from refresh and toggle results", async () => {
    const { ipc } = fakeIpc({
      ime_state: async () => ({ ...chinese }),
      ime_toggle: async () => ({ ...chinese, native: false, family: "zh" }),
    });
    const toggle = createImeToggle({ ipc, getMode: () => "on", showHud: vi.fn() });
    expect(toggle.family()).toBe(null);
    await toggle.refresh();
    expect(toggle.family()).toBe("zh");
    toggle.handleKeydown(keydown(), {});
    await flush();
    expect(toggle.family()).toBe("zh");
  });
});

describe("showImeBalloon", () => {
  // vitest 환경은 node(jsdom 없음): 풍선이 쓰는 DOM 표면만 흉내 낸다.
  type FakeEl = {
    className: string;
    textContent: string;
    title: string;
    style: Record<string, string>;
    children: FakeEl[];
    parent: FakeEl | null;
    addEventListener: () => void;
    appendChild(child: FakeEl): void;
    remove(): void;
    querySelector(selector: string): FakeEl | null;
    querySelectorAll(selector: string): FakeEl[];
  };
  const matches = (el: FakeEl, selector: string) =>
    el.className.split(/\s+/).includes(selector.replace(/^\./, ""));
  const makeEl = (className = ""): FakeEl => {
    const el: FakeEl = {
      className,
      textContent: "",
      title: "",
      style: {},
      children: [],
      parent: null,
      addEventListener: () => undefined,
      appendChild(child) {
        child.parent = el;
        el.children.push(child);
      },
      remove() {
        if (el.parent) el.parent.children = el.parent.children.filter((c) => c !== el);
      },
      querySelector(selector) {
        return el.querySelectorAll(selector)[0] ?? null;
      },
      querySelectorAll(selector) {
        return el.children.flatMap((c) => (matches(c, selector) ? [c] : []).concat(c.querySelectorAll(selector)));
      },
    };
    return el;
  };

  it("draws the real OS state next to the caret and never guesses", () => {
    vi.stubGlobal("document", { createElement: () => makeEl() });
    try {
      const helpers = makeEl("xterm-helpers");
      const element = makeEl("xterm");
      element.appendChild(helpers);
      const terminal = {
        element,
        buffer: { active: { cursorX: 3, cursorY: 2 } },
        _core: { _renderService: { dimensions: { css: { cell: { width: 10, height: 20 } } } } },
      };
      showImeBalloon(terminal, { available: true, native: true, family: "ko", sourceId: "x", reason: null });
      let balloon = helpers.querySelector(".ime-mode-balloon")!;
      expect(balloon.textContent).toBe("한");
      expect(balloon.className).toContain("ime-mode-ko");
      expect(balloon.style.left).toBe("26px");
      expect(balloon.style.top).toBe("40px");

      // 중국어·일본어 네이티브 글리프와 라틴 "A".
      showImeBalloon(terminal, { available: true, native: true, family: "zh", sourceId: "x", reason: null });
      balloon = helpers.querySelector(".ime-mode-balloon")!;
      expect(balloon.textContent).toBe("中");
      expect(balloon.className).toContain("ime-mode-zh");
      showImeBalloon(terminal, { available: true, native: true, family: "ja", sourceId: "x", reason: null });
      balloon = helpers.querySelector(".ime-mode-balloon")!;
      expect(balloon.textContent).toBe("あ");
      expect(balloon.className).toContain("ime-mode-ja");
      showImeBalloon(terminal, { available: true, native: false, family: "ko", sourceId: "x", reason: null });
      balloon = helpers.querySelector(".ime-mode-balloon")!;
      expect(balloon.textContent).toBe("A");
      expect(balloon.className).toContain("ime-mode-en");

      showImeBalloon(terminal, { available: true, native: null, family: null, sourceId: null, reason: "unreadable" });
      balloon = helpers.querySelector(".ime-mode-balloon")!;
      expect(balloon.textContent).toBe("?");
      expect(balloon.title).toBe("unreadable");

      showImeBalloon(terminal, { error: "boom" });
      balloon = helpers.querySelector(".ime-mode-balloon")!;
      expect(balloon.textContent).toBe("전환 ✕");
      expect(balloon.title).toBe("boom");
      expect(helpers.querySelectorAll(".ime-mode-balloon")).toHaveLength(1);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("is a no-op without a document or a terminal element", () => {
    expect(() => showImeBalloon({}, { error: "x" })).not.toThrow();
  });
});
