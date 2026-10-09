import { describe, expect, it, vi } from "vitest";
import type { IpcAdapter } from "../bridge/ipc";
import type { HangulToggleMode } from "../../store/preferences";
import { attachWebKitImeBridge, shiftEnterAction, windowsPtyOption } from "./xtermSetup";
import { DEL, HOLD_IME_WAIT_MS, HOLD_WINDOW_MS, detectImeEngine } from "./imeBridge";
import { createHangulToggle } from "./hangulToggle";

class FakeHost {
  private readonly listeners = new Map<string, EventListenerOrEventListenerObject[]>();

  constructor(
    private readonly textarea: { value: string },
    private readonly xtermEl: unknown = null,
  ) {}

  querySelector(selector?: string): unknown {
    // 브리지는 ".xterm"(캐럿 숨김 클래스 토글용)와 "textarea"를 각각 조회한다.
    if (selector === ".xterm") return this.xtermEl;
    return this.textarea;
  }

  addEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }

  removeEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
    this.listeners.set(type, (this.listeners.get(type) ?? []).filter((item) => item !== listener));
  }

  dispatch(type: string, event: Event): void {
    for (const listener of this.listeners.get(type) ?? []) {
      if (typeof listener === "function") listener(event);
      else listener.handleEvent(event);
    }
  }

  listenerCount(): number {
    let n = 0;
    for (const list of this.listeners.values()) n += list.length;
    return n;
  }
}

describe("attachWebKitImeBridge — 엔진 게이트(W3 Windows)", () => {
  const SAFARI = {
    vendor: "Apple Computer, Inc.",
    userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15",
  };
  const WEBVIEW2 = {
    vendor: "Google Inc.",
    userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36 Edg/126.0.0.0",
  };

  it("detectImeEngine: WKWebView/Safari만 webkit, Chromium 계열·node는 other", () => {
    expect(detectImeEngine(SAFARI)).toBe("webkit");
    expect(detectImeEngine(WEBVIEW2)).toBe("other");
    // WebKitGTK(Tauri on Ubuntu): Safari와 같은 vendor/UA 형태지만 IBus/fcitx의
    // 표준 조합 순서라 조정기 대상이 아니다 — 트레이스 확보 전까지 네이티브.
    expect(
      detectImeEngine({
        vendor: "Apple Computer, Inc.",
        userAgent:
          "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15",
      }),
    ).toBe("other");
    expect(detectImeEngine({ vendor: "Google Inc.", userAgent: "… Chrome/126 …" })).toBe("other");
    // vendor를 사칭해도 UA에 Chrome/Edg가 있으면 WebKit 폴백 IME가 아니다.
    expect(detectImeEngine({ vendor: "Apple Computer, Inc.", userAgent: "… Chrome/126 …" })).toBe("other");
    expect(detectImeEngine(undefined)).toBe("other");
    // node(vendor 없음)에서는 기본 판정이 other — 시험은 engine을 명시해야 한다.
    expect(detectImeEngine()).toBe("other");
  });

  it("Chromium 엔진에서는 리스너를 하나도 붙이지 않고 xterm 네이티브 조합기에 맡긴다", () => {
    const textarea = { value: "" };
    const host = new FakeHost(textarea);
    const triggerDataEvent = vi.fn();
    const terminal = { _core: { coreService: { triggerDataEvent } } };
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "other" });
    expect(host.listenerCount()).toBe(0);
    // Space keydown·입력 이벤트가 와도 우리는 아무것도 보내지 않는다(중복 방지).
    host.dispatch("keydown", keyboardEvent(32));
    host.dispatch("input", inputEvent("insertText", " "));
    expect(triggerDataEvent).not.toHaveBeenCalled();
    expect(() => detach()).not.toThrow();
  });

  it("engine 미지정이면 실행 환경을 판정한다 — node/Chromium은 no-op, WebKit은 부착", () => {
    const host = new FakeHost({ value: "" });
    const terminal = { _core: { coreService: { triggerDataEvent: vi.fn() } } };
    attachWebKitImeBridge(host as unknown as HTMLElement, terminal)();
    expect(host.listenerCount()).toBe(0);
    const attached = new FakeHost({ value: "" });
    const detach = attachWebKitImeBridge(attached as unknown as HTMLElement, terminal, { engine: "webkit" });
    expect(attached.listenerCount()).toBeGreaterThan(0);
    detach();
    expect(attached.listenerCount()).toBe(0);
  });

  it("windowsPty 옵션은 Windows에서만 ConPTY 힌트를 준다", () => {
    expect(windowsPtyOption("windows")).toEqual({ backend: "conpty" });
    expect(windowsPtyOption("darwin")).toBeUndefined();
    expect(windowsPtyOption("linux")).toBeUndefined();
  });
});

function keyboardEvent(
  keyCode: number,
  modifiers: Partial<Pick<KeyboardEvent, "ctrlKey" | "altKey" | "metaKey" | "shiftKey">> = {},
) {
  return {
    keyCode,
    isComposing: false,
    ctrlKey: false,
    altKey: false,
    metaKey: false,
    shiftKey: false,
    stopPropagation: vi.fn(),
    preventDefault: vi.fn(),
    ...modifiers,
  } as unknown as KeyboardEvent;
}

function inputEvent(inputType: string, data?: string) {
  return {
    inputType,
    data,
    stopPropagation: vi.fn(),
    preventDefault: vi.fn(),
  } as unknown as InputEvent;
}

function compositionEvent(type: string, data: string) {
  return { type, data } as unknown as CompositionEvent;
}

describe("Shift+Enter terminal mapping", () => {
  const event = {
    type: "keydown",
    key: "Enter",
    keyCode: 13,
    shiftKey: true,
    ctrlKey: false,
    altKey: false,
    metaKey: false,
    isComposing: false,
  };

  it("keydown은 LF 전송, keypress는 삼켜 CR 이중 전송(\\n\\r)을 막는다", () => {
    expect(shiftEnterAction(event)).toBe("send-lf");
    // 같은 Shift+Enter의 keypress는 xterm이 CR을 보내지 못하게 삼킨다.
    expect(shiftEnterAction({ ...event, type: "keypress" })).toBe("suppress");
  });

  it("IME 조합 중이거나 다른 수정키가 있으면 xterm의 기존 처리에 맡긴다", () => {
    expect(shiftEnterAction({ ...event, isComposing: true })).toBeNull();
    expect(shiftEnterAction({ ...event, ctrlKey: true })).toBeNull();
    expect(shiftEnterAction({ ...event, altKey: true })).toBeNull();
    expect(shiftEnterAction({ ...event, metaKey: true })).toBeNull();
    expect(shiftEnterAction({ ...event, type: "keyup" })).toBeNull();
    // Shift 없는 일반 Enter는 관여하지 않는다(제출 유지).
    expect(shiftEnterAction({ ...event, shiftKey: false })).toBeNull();
  });
});

describe("attachWebKitImeBridge — Space 단일 소유권", () => {
  it("xterm 전파만 막고 기본 textarea 삽입은 살려 공백을 정확히 한 번 보낸다", () => {
    const textarea = { value: "" };
    const host = new FakeHost(textarea);
    const triggerDataEvent = vi.fn();
    const terminal = { _core: { coreService: { triggerDataEvent } } };
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });

    const keydown = keyboardEvent(32);
    host.dispatch("keydown", keydown);
    expect(keydown.stopPropagation).toHaveBeenCalledOnce();
    expect(keydown.preventDefault).not.toHaveBeenCalled();

    const keypress = keyboardEvent(32);
    host.dispatch("keypress", keypress);
    expect(keypress.stopPropagation).toHaveBeenCalledOnce();

    const beforeinput = inputEvent("insertText", "\xa0");
    host.dispatch("beforeinput", beforeinput);
    textarea.value = "\xa0";
    const input = inputEvent("insertText", "\xa0");
    host.dispatch("input", input);
    expect(triggerDataEvent).toHaveBeenCalledTimes(1);
    expect(triggerDataEvent).toHaveBeenCalledWith(" ", true);
    expect(input.stopPropagation).toHaveBeenCalledOnce();
    expect(input.preventDefault).not.toHaveBeenCalled();

    detach();
    const afterDetach = keyboardEvent(32);
    host.dispatch("keydown", afterDetach);
    expect(afterDetach.stopPropagation).not.toHaveBeenCalled();
  });

  // NOTE: 예전 "확정된 한글을 textarea에서 비운다" 테스트는 clear 기능과 함께
  // 삭제했다. textarea를 ""로 비우면 WebKit 조합이 어긋나 다음 Backspace에서
  // 엉뚱한 자모(안아·하ㅎ)와 스페이스 유실이 났다. 이제 비우지 않고 꼬리 diff
  // (DEL+삽입)에 맡긴다 — \x7f가 Codex 등에서 정상 동작한다.

  it("확정 마커 뒤에도 조합이 이어지면 textarea를 비우지 않아 자모가 새지 않는다", () => {
    // 실제 WebKit(두벌식) 순서: 안 확정 마커 → 곧바로 ㄴ·ㅕ·ㅇ로 녕을 같은
    // textarea에 이어 조합. 마커에서 비우면 녕이 ㄴㅕㅇ로 분해돼 샌다.
    const textarea = { value: "" };
    const host = new FakeHost(textarea);
    const triggerDataEvent = vi.fn();
    const terminal = { _core: { coreService: { triggerDataEvent } } };
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });

    const step = (type: string, value: string): void => {
      host.dispatch("beforeinput", inputEvent(type, value));
      textarea.value = value;
      host.dispatch("input", inputEvent(type, value));
    };
    step("insertText", "ㅇ");
    step("insertReplacementText", "아");
    step("insertReplacementText", "안");
    // 값 불변 확정 마커 — 여기서 비우면 안 된다.
    host.dispatch("beforeinput", inputEvent("insertReplacementText", "안"));
    host.dispatch("input", inputEvent("insertReplacementText", "안"));
    expect(textarea.value).toBe("안"); // 예약만, 보존
    // 다음 음절이 같은 textarea에 이어진다.
    step("insertText", "안ㄴ");
    step("insertReplacementText", "안녀");
    step("insertReplacementText", "안녕");
    // Enter로 확정.
    host.dispatch("keydown", keyboardEvent(13));

    const emitted = triggerDataEvent.mock.calls.map(([data]) => data).join("");
    expect(emitted).toBe("안녕"); // ㄴㅕㅇ 자모 유출 없음
    detach();
  });

  it("조합 중 .xterm에 .ime-composing을 부여하고 확정 시 제거한다(캐럿 숨김)", () => {
    const textarea = { value: "" };
    const classSet = new Set<string>();
    const xtermEl = {
      classList: {
        toggle: (c: string, on: boolean): void => {
          if (on) classSet.add(c);
          else classSet.delete(c);
        },
      },
    };
    const host = new FakeHost(textarea, xtermEl);
    const compositionView = {
      textContent: "",
      style: { letterSpacing: "" },
      classList: { add: vi.fn(), remove: vi.fn() },
    };
    const triggerDataEvent = vi.fn();
    const terminal = {
      _core: {
        coreService: { triggerDataEvent },
        _compositionHelper: {
          _isComposing: false,
          _compositionView: compositionView,
          updateCompositionElements: vi.fn(),
        },
      },
    };
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });

    // 조합 시작 → 캐럿 숨김 클래스 부여
    host.dispatch("beforeinput", inputEvent("insertText", "ㅇ"));
    textarea.value = "ㅇ";
    host.dispatch("input", inputEvent("insertText", "ㅇ"));
    expect(classSet.has("ime-composing")).toBe(true);

    // Enter로 확정 → 클래스 제거(커서 다시 보임)
    host.dispatch("keydown", keyboardEvent(13));
    expect(classSet.has("ime-composing")).toBe(false);
    detach();
  });

  it("조합 중 커서 이동 키는 대기 조합을 확정한다(textarea는 건드리지 않는다)", () => {
    // 확정 후 textarea를 비우는 clear는 WebKit 조합과 어긋나 엉뚱한 자모를
    // 만들어 제거했다. 방향키는 실제 키이므로 대기 조합만 확정(전송)한다.
    const textarea = { value: "" };
    const host = new FakeHost(textarea);
    const triggerDataEvent = vi.fn();
    const terminal = { _core: { coreService: { triggerDataEvent } } };
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });
    host.dispatch("beforeinput", inputEvent("insertText", "ㅇ"));
    textarea.value = "ㅇ";
    host.dispatch("input", inputEvent("insertText", "ㅇ"));
    host.dispatch("beforeinput", inputEvent("insertReplacementText", "아"));
    textarea.value = "아";
    host.dispatch("input", inputEvent("insertReplacementText", "아"));
    host.dispatch("keydown", keyboardEvent(39)); // ArrowRight
    expect(triggerDataEvent).toHaveBeenLastCalledWith("아", true);
    expect(textarea.value).toBe("아"); // WebKit 소유 — 비우지 않는다
    detach();
  });

  it("한글 뒤 스페이스가 유실되지 않는다(스페이스 먹힘 회귀)", () => {
    // 회귀: 확정 emit '전'에 textarea를 비우면 스페이스가 담긴 값이 지워져
    // 셸로 안 나갔다. emit 이후에만 비워야 한다.
    const textarea = { value: "" };
    const host = new FakeHost(textarea);
    const triggerDataEvent = vi.fn();
    const terminal = { _core: { coreService: { triggerDataEvent } } };
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });

    const step = (type: string, value: string): void => {
      host.dispatch("beforeinput", inputEvent(type, value));
      textarea.value = value;
      host.dispatch("input", inputEvent(type, value));
    };
    step("insertText", "ㅎ");
    step("insertReplacementText", "하");
    step("insertReplacementText", "한");
    // 스페이스: 브리지가 keydown에서 앞 음절(한)을 확정하고, 기본 textarea
    // 삽입(input)이 스페이스를 셸로 보낸다. WebKit은 NBSP로 넣지만 여기선 공백.
    host.dispatch("keydown", keyboardEvent(32));
    host.dispatch("keypress", keyboardEvent(32));
    host.dispatch("beforeinput", inputEvent("insertText", " "));
    textarea.value = "한 ";
    host.dispatch("input", inputEvent("insertText", " "));
    host.dispatch("keyup", keyboardEvent(32));

    expect(triggerDataEvent.mock.calls.map(([data]) => data).join("")).toBe("한 ");
    detach();
  });

  it("NBSP 스페이스도 일반 공백으로 정확히 한 번 전송된다", () => {
    const textarea = { value: "" };
    const host = new FakeHost(textarea);
    const triggerDataEvent = vi.fn();
    const terminal = { _core: { coreService: { triggerDataEvent } } };
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });

    const step = (type: string, value: string): void => {
      host.dispatch("beforeinput", inputEvent(type, value));
      textarea.value = value;
      host.dispatch("input", inputEvent(type, value));
    };
    step("insertText", "ㅎ");
    step("insertReplacementText", "하");
    step("insertReplacementText", "한");
    host.dispatch("keydown", keyboardEvent(32));
    host.dispatch("keypress", keyboardEvent(32));
    host.dispatch("beforeinput", inputEvent("insertText", "\xa0"));
    textarea.value = "한\xa0";
    host.dispatch("input", inputEvent("insertText", "\xa0"));
    host.dispatch("keyup", keyboardEvent(32));

    const calls = triggerDataEvent.mock.calls.map(([data]) => data);
    expect(calls.join("")).toBe("한 "); // NBSP → 일반 공백, 중복 없음
    detach();
  });

  it("다음 음절이 조합 중이면 앞 음절을 보내고 textarea의 preedit은 보존한다", () => {
    const textarea = { value: "" };
    const host = new FakeHost(textarea);
    const triggerDataEvent = vi.fn();
    const terminal = { _core: { coreService: { triggerDataEvent } } };
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });

    host.dispatch("beforeinput", inputEvent("insertText", "ㅇ"));
    textarea.value = "ㅇ";
    host.dispatch("input", inputEvent("insertText", "ㅇ"));
    host.dispatch("beforeinput", inputEvent("insertReplacementText", "안"));
    textarea.value = "안";
    host.dispatch("input", inputEvent("insertReplacementText", "안"));
    host.dispatch("beforeinput", inputEvent("insertText", "ㄴ"));
    textarea.value = "안ㄴ";
    host.dispatch("input", inputEvent("insertText", "ㄴ"));

    expect(triggerDataEvent).toHaveBeenLastCalledWith("안", true);
    expect(textarea.value).toBe("안ㄴ");
    detach();
  });

  it("표준 composition 이벤트가 오면 폴백이 xterm의 네이티브 preedit을 덮어쓰지 않는다", () => {
    const textarea = { value: "이제는 " };
    const host = new FakeHost(textarea);
    const compositionView = {
      textContent: "",
      style: { letterSpacing: "" },
      classList: { add: vi.fn(), remove: vi.fn() },
    };
    const triggerDataEvent = vi.fn();
    const terminal = {
      _core: {
        coreService: { triggerDataEvent },
        _compositionHelper: {
          _isComposing: false,
          _compositionView: compositionView,
          updateCompositionElements: vi.fn(),
        },
      },
    };
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });

    host.dispatch("compositionstart", compositionEvent("compositionstart", ""));
    host.dispatch("compositionupdate", compositionEvent("compositionupdate", "겹"));
    // target 단계에서 xterm이 현재 음절만 표시한 상황을 재현한다.
    compositionView.textContent = "겹";
    host.dispatch("beforeinput", inputEvent("insertCompositionText", "겹"));
    textarea.value = "이제는 겹";
    host.dispatch("input", inputEvent("insertCompositionText", "겹"));

    expect(compositionView.textContent).toBe("겹");
    const composingKeydown = keyboardEvent(229);
    host.dispatch("keydown", composingKeydown);
    expect(composingKeydown.stopPropagation).not.toHaveBeenCalled();
    expect(triggerDataEvent).not.toHaveBeenCalled();

    host.dispatch("compositionend", compositionEvent("compositionend", "겹"));
    const spaceKeydown = keyboardEvent(32);
    host.dispatch("keydown", spaceKeydown);
    host.dispatch("keypress", keyboardEvent(32));
    host.dispatch("beforeinput", inputEvent("insertText", "\xa0"));
    textarea.value = "이제는 겹\xa0";
    host.dispatch("input", inputEvent("insertText", "\xa0"));

    expect(spaceKeydown.stopPropagation).toHaveBeenCalledOnce();
    expect(triggerDataEvent).toHaveBeenLastCalledWith(" ", true);
    detach();
  });

  it("한글 조합 중 Shift+Enter는 조합만 먼저 확정하고 키 이벤트는 xterm에 전달한다", () => {
    const textarea = { value: "" };
    const host = new FakeHost(textarea);
    const triggerDataEvent = vi.fn();
    const terminal = { _core: { coreService: { triggerDataEvent } } };
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });

    host.dispatch("beforeinput", inputEvent("insertText", "ㅇ"));
    textarea.value = "ㅇ";
    host.dispatch("input", inputEvent("insertText", "ㅇ"));
    host.dispatch("beforeinput", inputEvent("insertReplacementText", "안"));
    textarea.value = "안";
    host.dispatch("input", inputEvent("insertReplacementText", "안"));

    const shiftEnter = keyboardEvent(13, { shiftKey: true });
    host.dispatch("keydown", shiftEnter);

    expect(triggerDataEvent).toHaveBeenLastCalledWith("안", true);
    expect(shiftEnter.stopPropagation).not.toHaveBeenCalled();
    expect(shiftEnter.preventDefault).not.toHaveBeenCalled();
    detach();
  });

  it("Ctrl/Alt/Meta Space는 브리지가 가로채지 않는다", () => {
    for (const modifiers of [{ ctrlKey: true }, { altKey: true }, { metaKey: true }]) {
      const textarea = { value: "" };
      const host = new FakeHost(textarea);
      const terminal = { _core: { coreService: { triggerDataEvent: vi.fn() } } };
      const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });
      const keydown = keyboardEvent(32, modifiers);

      host.dispatch("keydown", keydown);

      expect(keydown.stopPropagation).not.toHaveBeenCalled();
      detach();
    }
  });
});

describe("attachWebKitImeBridge — 유휴 확정 뒤 Backspace 분해(하ㅎ 겹침 회귀)", () => {
  it("백스톱이 보낸 하를 IME가 ㅎ으로 분해하면 DEL을 즉시 보내고 preedit만 ㅎ이 된다", () => {
    vi.useFakeTimers();
    try {
      const textarea = { value: "" };
      const host = new FakeHost(textarea);
      const compositionView = {
        textContent: "",
        style: { letterSpacing: "" },
        classList: { add: vi.fn(), remove: vi.fn() },
      };
      const triggerDataEvent = vi.fn();
      const terminal = {
        _core: {
          coreService: { triggerDataEvent },
          _compositionHelper: {
            _isComposing: false,
            _compositionView: compositionView,
            updateCompositionElements: vi.fn(),
          },
        },
      };
      const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });
      const sent = (): string => triggerDataEvent.mock.calls.map(([data]) => data).join("");
      const step = (type: string, value: string, data: string): void => {
        host.dispatch("beforeinput", inputEvent(type, data));
        textarea.value = value;
        host.dispatch("input", inputEvent(type, data));
      };

      step("insertText", "ㅎ", "ㅎ");
      host.dispatch("keydown", { ...keyboardEvent(229), key: "ㅎ" } as unknown as KeyboardEvent);
      step("insertReplacementText", "하", "하");
      host.dispatch("keydown", { ...keyboardEvent(229), key: "ㅏ" } as unknown as KeyboardEvent);
      expect(compositionView.textContent).toBe("하");
      expect(sent()).toBe("");

      vi.advanceTimersByTime(2000); // 2초 유휴 → 백스톱이 하를 셸로 보낸다("다음 칸으로 이동")
      expect(sent()).toBe("하");
      expect(compositionView.textContent).toBe("");

      // WebKit IME는 여전히 하를 조합 중 — Backspace가 ㅎ으로 분해되어 온다
      // (실기 순서: insertReplacementText → keydown 229 "Backspace").
      step("insertReplacementText", "ㅎ", "ㅎ");
      host.dispatch("keydown", { ...keyboardEvent(229), key: "Backspace" } as unknown as KeyboardEvent);
      expect(sent()).toBe(`하${DEL}`); // 화면의 하를 즉시 걷어낸다(겹침 없음)
      expect(compositionView.textContent).toBe("ㅎ"); // preedit만 ㅎ

      // 조합이 비는 Backspace: 값 불변 마커 → 보류분 ㅎ 전송, 그 뒤 실제 keydown 8은
      // xterm이 DEL로 보낸다(여기서는 브리지가 막지 않는지만 본다).
      host.dispatch("beforeinput", inputEvent("insertReplacementText", "ㅎ"));
      host.dispatch("input", inputEvent("insertReplacementText", "ㅎ"));
      expect(sent()).toBe(`하${DEL}ㅎ`);
      const realBackspace = keyboardEvent(8);
      host.dispatch("keydown", realBackspace);
      expect(realBackspace.stopPropagation).not.toHaveBeenCalled();
      expect(compositionView.textContent).toBe("");
      detach();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("attachWebKitImeBridge — blur로 xterm이 textarea를 비울 때(ccd→cc 회귀)", () => {
  function setup() {
    const textarea = { value: "" };
    const host = new FakeHost(textarea);
    const compositionView = {
      textContent: "",
      style: { letterSpacing: "" },
      classList: { add: vi.fn(), remove: vi.fn() },
    };
    const triggerDataEvent = vi.fn();
    const terminal = {
      _core: {
        coreService: { triggerDataEvent },
        _compositionHelper: {
          _isComposing: false,
          _compositionView: compositionView,
          updateCompositionElements: vi.fn(),
        },
      },
    };
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });
    const sent = (): string => triggerDataEvent.mock.calls.map(([data]) => data).join("");
    const step = (type: string, value: string, data: string): void => {
      host.dispatch("beforeinput", inputEvent(type, data));
      textarea.value = value;
      host.dispatch("input", inputEvent(type, data));
    };
    const space = (): void => {
      host.dispatch("keydown", keyboardEvent(32));
      host.dispatch("keypress", keyboardEvent(32));
      host.dispatch("beforeinput", inputEvent("insertText", "\xa0"));
      textarea.value += "\xa0";
      host.dispatch("input", inputEvent("insertText", "\xa0"));
      host.dispatch("keyup", keyboardEvent(32));
    };
    return { textarea, host, compositionView, sent, step, space, detach };
  }

  it("확정 텍스트가 남은 채 blur → xterm 비움 → 영문 ccd + 스페이스: DEL 없이 공백만 보낸다", () => {
    vi.useFakeTimers();
    try {
      const t = setup();
      t.step("insertText", "ㅇ", "ㅇ");
      t.step("insertReplacementText", "안", "안");
      t.host.dispatch("beforeinput", inputEvent("insertReplacementText", "안"));
      t.host.dispatch("input", inputEvent("insertReplacementText", "안")); // 값 불변 마커 → 확정
      expect(t.sent()).toBe("안");

      // 사용자가 클릭/패널 전환: capture blur(브리지) → xterm _handleTextAreaBlur가 비움.
      t.host.dispatch("blur", {} as Event);
      t.textarea.value = "";
      vi.advanceTimersByTime(0);

      // 영문 c c d — xterm이 keydown에서 직접 보낸다(브리지는 막지 않고, textarea엔 안 들어간다).
      for (const code of [67, 67, 68]) {
        const keydown = keyboardEvent(code);
        t.host.dispatch("keydown", keydown);
        expect(keydown.stopPropagation).not.toHaveBeenCalled();
        t.host.dispatch("keyup", keyboardEvent(code));
      }
      t.space();
      expect(t.sent()).toBe("안 "); // 옛 "안"을 DEL로 되감지 않는다(ccd는 xterm 몫)
      t.detach();
    } finally {
      vi.useRealTimers();
    }
  });

  it("blur 이벤트를 못 봐도(프로그램적 비우기) 다음 beforeinput에서 자가 복구한다", () => {
    const t = setup();
    t.step("insertText", "ㅇ", "ㅇ");
    t.step("insertReplacementText", "안", "안");
    t.host.dispatch("keydown", keyboardEvent(39)); // 방향키 → 안 확정
    expect(t.sent()).toBe("안");
    t.textarea.value = ""; // blur 없이 비워짐
    t.space();
    expect(t.sent()).toBe("안 ");
    t.detach();
  });

  it("조합 보류 중 blur는 보류분을 먼저 내보내고 preedit을 정리한다", () => {
    vi.useFakeTimers();
    try {
      const t = setup();
      t.step("insertText", "ㅎ", "ㅎ");
      t.step("insertReplacementText", "하", "하");
      expect(t.compositionView.textContent).toBe("하");
      t.host.dispatch("blur", {} as Event);
      expect(t.sent()).toBe("하"); // 사용자가 본 preedit이 유실되지 않는다
      t.textarea.value = "";
      vi.advanceTimersByTime(0);
      expect(t.compositionView.textContent).toBe("");
      vi.advanceTimersByTime(3000); // 백스톱이 옛 값을 다시 보내지 않는다
      expect(t.sent()).toBe("하");
      t.detach();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("attachWebKitImeBridge — 입력 소스 홀드 창(시작·CapsLock·Unidentified)", () => {
  function setup() {
    const textarea = { value: "" };
    const host = new FakeHost(textarea);
    const triggerDataEvent = vi.fn();
    const terminal = { _core: { coreService: { triggerDataEvent } } };
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, { engine: "webkit" });
    const sent = (): string => triggerDataEvent.mock.calls.map(([data]) => data).join("");
    const kd = (keyCode: number, key: string, extra: Partial<KeyboardEvent> = {}) => {
      const ev = { ...keyboardEvent(keyCode, extra), key } as unknown as KeyboardEvent;
      host.dispatch("keydown", ev);
      return ev;
    };
    const ku = (keyCode: number, key: string): void => {
      host.dispatch("keyup", { ...keyboardEvent(keyCode), key } as unknown as KeyboardEvent);
    };
    return { textarea, host, sent, kd, ku, detach };
  }

  it("부착 직후 첫 라틴 키는 keyup까지만 보류된다(영문이면 방출, xterm은 보지 않는다)", () => {
    vi.useFakeTimers();
    try {
      const t = setup();
      const h = t.kd(72, "h");
      expect(h.stopPropagation).toHaveBeenCalledOnce();
      expect(h.preventDefault).toHaveBeenCalledOnce();
      expect(t.sent()).toBe("");
      t.ku(72, "h");
      expect(t.sent()).toBe("h"); // keyup의 라틴 key → 즉시 방출
      // 창이 닫혔으니 다음 키는 xterm 몫.
      const e = t.kd(69, "e");
      expect(e.stopPropagation).not.toHaveBeenCalled();
      t.detach();
    } finally {
      vi.useRealTimers();
    }
  });

  it("부착 직후 IME 우회 자모 keydown은 오토마타가 즉시 조립해 보낸다(실기 시작 경주 형태 A)", () => {
    vi.useFakeTimers();
    try {
      const t = setup();
      for (const [code, jamo] of [[68, "ㅇ"], [75, "ㅏ"], [83, "ㄴ"]] as Array<[number, string]>) {
        const ev = t.kd(code, jamo);
        expect(ev.stopPropagation).toHaveBeenCalledOnce(); // xterm이 자모를 그대로 보내지 못하게
        t.ku(code, jamo);
      }
      expect(t.sent()).toBe(`ㅇ${DEL}아${DEL}안`);
      t.detach();
    } finally {
      vi.useRealTimers();
    }
  });

  it("라틴 keydown의 keyup이 자모면 IME 삽입을 기다리고, 타이머 만료 시 자모로 복원한다(형태 B 지연)", () => {
    vi.useFakeTimers();
    try {
      const t = setup();
      t.kd(68, "d");
      t.ku(68, "ㅇ");
      expect(t.sent()).toBe("");
      vi.advanceTimersByTime(HOLD_IME_WAIT_MS + 1);
      expect(t.sent()).toBe("ㅇ"); // dk 유출 대신 자모
      t.detach();
    } finally {
      vi.useRealTimers();
    }
  });

  it("CapsLock은 방향과 무관하게 홀드 창을 열고, `Unidentified` keydown은 창을 해제하지 않는다", () => {
    vi.useFakeTimers();
    try {
      const t = setup();
      vi.advanceTimersByTime(HOLD_WINDOW_MS + 1); // 시작 창 만료
      t.kd(20, "CapsLock");
      t.kd(0, "Unidentified"); // 실기: CapsLock 16ms 뒤 — 종전엔 여기서 홀드가 풀렸다
      t.ku(20, "CapsLock");
      const d = t.kd(68, "d");
      expect(d.stopPropagation).toHaveBeenCalledOnce();
      expect(d.preventDefault).toHaveBeenCalledOnce();
      // IME가 활성화되어 같은 키를 조합으로 넣는다 → 보류 d는 버려진다(dk 유출 없음).
      t.host.dispatch("beforeinput", inputEvent("insertText", "ㅇ"));
      t.textarea.value = "ㅇ";
      t.host.dispatch("input", inputEvent("insertText", "ㅇ"));
      t.ku(68, "ㅇ");
      t.host.dispatch("keydown", keyboardEvent(13)); // Enter → 확정
      expect(t.sent()).toBe("ㅇ");
      t.detach();
    } finally {
      vi.useRealTimers();
    }
  });

  it("영문 모드에서 CapsLock 없이 타이핑하면 시작 창이 닫힌 뒤엔 어떤 지연도 없다", () => {
    vi.useFakeTimers();
    try {
      const t = setup();
      vi.advanceTimersByTime(HOLD_WINDOW_MS + 1);
      const a = t.kd(65, "a");
      expect(a.stopPropagation).not.toHaveBeenCalled();
      expect(a.preventDefault).not.toHaveBeenCalled();
      t.detach();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("attachWebKitImeBridge — 커서 이동 키의 IME 선택 범위 흔들기", () => {
  it("방향키·Home/End·PgUp/PgDn에서 캐럿을 0→끝으로 옮겨 IME 마킹을 폐기시킨다(값은 불변)", () => {
    const calls: Array<[number, number]> = [];
    const textarea = {
      value: "안녕",
      setSelectionRange: (a: number, b: number): void => {
        calls.push([a, b]);
      },
    };
    const host = new FakeHost(textarea);
    const triggerDataEvent = vi.fn();
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, { _core: { coreService: { triggerDataEvent } } }, { engine: "webkit" });
    host.dispatch("keydown", keyboardEvent(37)); // ArrowLeft — xterm 몫(차단 안 함)
    expect(calls).toEqual([[0, 0], [2, 2]]);
    expect(textarea.value).toBe("안녕"); // 불변식 4: 값은 비우지 않는다
    calls.length = 0;
    host.dispatch("keydown", keyboardEvent(65)); // 일반 키는 흔들지 않는다
    host.dispatch("keydown", keyboardEvent(37, { altKey: true })); // Alt+←(단어 이동)도 IME와 무관
    expect(calls).toEqual([]);
    detach();
  });

  it("setSelectionRange가 없는 textarea(테스트 더블 등)에서는 조용히 넘어간다", () => {
    const textarea = { value: "안" };
    const host = new FakeHost(textarea);
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, { _core: { coreService: { triggerDataEvent: vi.fn() } } }, { engine: "webkit" });
    expect(() => host.dispatch("keydown", keyboardEvent(39))).not.toThrow();
    detach();
  });
});

describe("attachWebKitImeBridge — Shift+Space 한/영 강제 전환(capture에서 코어보다 먼저)", () => {
  // 실기 트레이스 회귀: `kd 32 shift=true -> block` 뒤 공백 전송. 브리지가 Space를
  // xterm보다 먼저 차단해 xterm custom key handler의 전환 코드가 불리지 않았다.
  const shiftSpace = (): KeyboardEvent =>
    ({
      ...keyboardEvent(32, { shiftKey: true }),
      type: "keydown",
      code: "Space",
      key: " ",
      repeat: false,
    }) as unknown as KeyboardEvent;

  function setup(mode: HangulToggleMode) {
    const textarea = { value: "" };
    const host = new FakeHost(textarea);
    const triggerDataEvent = vi.fn();
    const terminal = { _core: { coreService: { triggerDataEvent } } };
    const calls: string[] = [];
    const ipc: IpcAdapter = {
      invoke<T>(command: string): Promise<T> {
        calls.push(command);
        return Promise.resolve({
          available: true,
          hangul: false,
          sourceId: "com.apple.keylayout.ABC",
          reason: null,
        } as unknown as T);
      },
      channel(): unknown {
        return {};
      },
    };
    const hangulToggle = createHangulToggle({ ipc, getMode: () => mode, showHud: vi.fn() });
    const detach = attachWebKitImeBridge(host as unknown as HTMLElement, terminal, {
      engine: "webkit",
      hangulToggle,
    });
    const sent = (): string => triggerDataEvent.mock.calls.map(([data]) => data).join("");
    const step = (type: string, value: string): void => {
      host.dispatch("beforeinput", inputEvent(type, value));
      textarea.value = value;
      host.dispatch("input", inputEvent(type, value));
    };
    const kd = (keyCode: number, key: string): KeyboardEvent => {
      const ev = { ...keyboardEvent(keyCode), key } as unknown as KeyboardEvent;
      host.dispatch("keydown", ev);
      return ev;
    };
    const ku = (keyCode: number, key: string): void => {
      host.dispatch("keyup", { ...keyboardEvent(keyCode), key } as unknown as KeyboardEvent);
    };
    return { host, sent, step, kd, ku, calls, detach };
  }

  it("켜져 있으면 공백 대신 OS 전환을 요청하고, 조합을 먼저 확정한 뒤 전환 직후 첫 키를 홀드 창으로 가른다", () => {
    vi.useFakeTimers();
    try {
      const t = setup("on");
      vi.advanceTimersByTime(HOLD_WINDOW_MS + 1); // 부착 홀드 창 만료
      t.step("insertText", "ㅎ");
      t.step("insertReplacementText", "하");
      expect(t.sent()).toBe(""); // 조합 보류 중
      const chord = shiftSpace();
      t.host.dispatch("keydown", chord);
      expect(chord.preventDefault).toHaveBeenCalledOnce(); // textarea 공백 삽입·keypress 차단
      expect(chord.stopPropagation).toHaveBeenCalledOnce(); // xterm은 보지 않는다
      expect(t.calls).toEqual(["ime_toggle_hangul"]);
      expect(t.sent()).toBe("하"); // 전환 전에 조합 확정 — 공백은 없다
      const l = t.kd(76, "l");
      expect(l.preventDefault).toHaveBeenCalledOnce(); // 전환 직후 라틴 키는 keyup 판정까지 보류
      expect(t.sent()).toBe("하");
      t.ku(76, "l");
      expect(t.sent()).toBe("하l");
      t.detach();
    } finally {
      vi.useRealTimers();
    }
  });

  it("꺼져 있으면 종전 Space 단일 소유 그대로 — 공백을 정확히 한 번 보내고 OS 전환은 요청하지 않는다", () => {
    const t = setup("off");
    const chord = shiftSpace();
    t.host.dispatch("keydown", chord);
    expect(chord.stopPropagation).toHaveBeenCalledOnce();
    expect(chord.preventDefault).not.toHaveBeenCalled();
    t.step("insertText", "\xa0");
    expect(t.sent()).toBe(" ");
    expect(t.calls).toEqual([]);
    t.detach();
  });
});
