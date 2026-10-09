import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { attachWebKitImeBridge } from "./xtermSetup";

/**
 * 실기 트레이스 리플레이 — macOS WKWebView(두벌식) 실사용 세션에서 브리지의
 * note() 트레이스(beforeinput/input/keydown/keyup/keypress + 타임스탬프)를 그대로
 * 저장한 fixture를 attach 계층에 다시 흘려 넣는다. 합성 이벤트로는 재현이 어려운
 * 실제 WebKit 순서가 모두 들어 있다: 조합(insertReplacementText), 값 불변 확정
 * 마커, Backspace 자소 분해(keyCode 229), 조합이 비워질 때의 빈 마커 뒤 실제
 * Backspace(keyCode 8), NBSP 스페이스, CapsLock 한/영 전환(홀드 창), 2초 백스톱
 * 타이머, 사용자의 오타 수정(연속 Backspace), 앱 시작 직후 IME 우회(자모 keydown),
 * 라틴 keydown 뒤 늦게 도착하는 IME 삽입 등.
 *
 * 브리지가 막지 않은 이벤트는 가짜 xterm이 **xterm 6.0의 실제 규칙**대로 처리한다
 * (아래 FakeXterm). 기대값은 사용자가 Enter로 보낸 문장 그 자체 — PTY가 받은
 * 바이트에 DEL/CR을 적용한 결과가 이와 정확히 같아야 한다.
 *
 * fixture 형식(탭 구분, ts는 첫 이벤트 기준 ms):
 *   ts  kd   keyCode  key(JSON)  shift(0/1)  mod(0/1)
 *   ts  ku   keyCode  [key(JSON)]
 *   ts  kp   keyCode  [key(JSON)]
 *   ts  bi   inputType  data(JSON|null)
 *   ts  inp  inputType  data(JSON|null)  tailDeletes  tailInsert(JSON)
 * inp의 tailDeletes/tailInsert는 직전 textarea 값에 대한 꼬리 편집이다.
 * (로그 → fixture 변환: 브리지 트레이스의 kd/ku/kp/bi/inp 줄을 그대로 옮기고
 * inp의 value=로 꼬리 편집을 계산한다.)
 */

const DEL = "\x7f";
const ESC = "\x1b";

interface StoppableEvent {
  propagationStopped: boolean;
  defaultPrevented: boolean;
  stopPropagation(): void;
  preventDefault(): void;
}

function stoppable<T extends object>(fields: T): T & StoppableEvent {
  const ev = {
    ...fields,
    propagationStopped: false,
    defaultPrevented: false,
    stopPropagation(): void {
      ev.propagationStopped = true;
    },
    preventDefault(): void {
      ev.defaultPrevented = true;
    },
  };
  return ev;
}

class FakeHost {
  private readonly listeners = new Map<string, EventListenerOrEventListenerObject[]>();

  constructor(private readonly textarea: { value: string }) {}

  querySelector(selector?: string): unknown {
    if (selector === ".xterm") return null;
    return this.textarea;
  }

  addEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }

  removeEventListener(type: string, listener: EventListenerOrEventListenerObject): void {
    this.listeners.set(type, (this.listeners.get(type) ?? []).filter((item) => item !== listener));
  }

  /** 등록 순서대로 호출 — 브리지(capture)가 먼저, 가짜 xterm(target)이 뒤. */
  dispatch(type: string, event: unknown): void {
    for (const listener of this.listeners.get(type) ?? []) {
      if (typeof listener === "function") listener(event as Event);
      else listener.handleEvent(event as Event);
    }
  }
}

type KeyEv = KeyboardEvent & StoppableEvent;
type InputEv = InputEvent & StoppableEvent;

/**
 * xterm 6.0 CoreBrowserTerminal의 키/입력 처리 중 PTY 전송에 관여하는 부분만
 * 충실히 흉내 낸다(브리지가 stopPropagation 하지 않은 이벤트만 본다):
 *  - keydown: `_keyDownSeen=true`. Backspace→DEL, Enter→CR + textarea 비움,
 *    Tab/Esc/방향키는 제어열. 수정키 없는 단일 문자(keyCode ≥ 48)는 **keydown에서
 *    즉시 전송하고 preventDefault**(Keyboard.ts default 분기 + cancel(force)).
 *    단, A–Z 대문자는 macOS IME 핵(HACK)으로 keypress에 넘긴다.
 *  - keyCode 229: CompositionHelper.keydown → `_handleAnyTextareaChanges`:
 *    setTimeout(0) 뒤 textarea 값 차이를 전송한다(브리지가 막지 않으면 조합이
 *    중복 전송되는 경로 — 회귀 검출용).
 *  - keypress: `_keyDownHandled`가 아니면 문자를 전송한다. cancelEvents 기본
 *    false라 preventDefault 하지 않는다(= 브라우저 기본 삽입이 일어나 input이
 *    뒤따른다).
 *  - input: insertText이고 `!_keyDownSeen`이며 keypress가 처리하지 않았으면
 *    data를 전송한다(_inputEvent).
 *  - keyup: `_keyDownSeen=false`, `_keyPressHandled=false`.
 */
class FakeXterm {
  private keyDownSeen = false;
  private keyDownHandled = false;
  private keyPressHandled = false;

  constructor(
    host: FakeHost,
    private readonly textarea: { value: string },
    private readonly send: (data: string) => void,
  ) {
    host.addEventListener("keydown", ((ev: KeyEv) => this.keydown(ev)) as unknown as EventListener);
    host.addEventListener("keypress", ((ev: KeyEv) => this.keypress(ev)) as unknown as EventListener);
    host.addEventListener("input", ((ev: InputEv) => this.input(ev)) as unknown as EventListener);
    host.addEventListener("keyup", ((ev: KeyEv) => this.keyup(ev)) as unknown as EventListener);
  }

  private keydown(ev: KeyEv): void {
    if (ev.propagationStopped) return;
    this.keyDownHandled = false;
    this.keyDownSeen = true;
    const mod = ev.ctrlKey || ev.altKey || ev.metaKey;
    if (ev.keyCode === 229) {
      const oldValue = this.textarea.value;
      setTimeout(() => {
        const newValue = this.textarea.value;
        const diff = newValue.replace(oldValue, "");
        if (newValue.length > oldValue.length) this.send(diff);
        else if (newValue.length < oldValue.length) this.send(DEL);
        else if (newValue !== oldValue) this.send(newValue);
      }, 0);
      return;
    }
    let key: string | null = null;
    switch (ev.keyCode) {
      case 8:
        key = ev.altKey ? ESC + DEL : DEL;
        break;
      case 9:
        key = "\t";
        break;
      case 13:
        key = "\r";
        this.textarea.value = "";
        break;
      case 27:
        key = ESC;
        break;
      case 37:
        key = `${ESC}[D`;
        break;
      case 38:
        key = `${ESC}[A`;
        break;
      case 39:
        key = `${ESC}[C`;
        break;
      case 40:
        key = `${ESC}[B`;
        break;
      default:
        if (ev.ctrlKey && ev.keyCode >= 65 && ev.keyCode <= 90) {
          key = String.fromCharCode(ev.keyCode - 64);
          if (ev.keyCode === 67) this.textarea.value = "";
        } else if (!mod && typeof ev.key === "string" && ev.key.length === 1 && ev.keyCode >= 48) {
          key = ev.key;
        }
    }
    if (key === null) return;
    // macOS IME HACK: A–Z 대문자는 keypress에서 처리한다.
    if (!mod && typeof ev.key === "string" && ev.key.length === 1) {
      const code = ev.key.charCodeAt(0);
      if (code >= 65 && code <= 90) return;
    }
    this.send(key);
    ev.preventDefault();
    this.keyDownHandled = true;
  }

  private keypress(ev: KeyEv): void {
    if (ev.propagationStopped) return;
    this.keyPressHandled = false;
    if (this.keyDownHandled) return;
    const code = typeof ev.key === "string" && ev.key.length === 1 ? ev.key.charCodeAt(0) : ev.keyCode;
    if (!code || ev.altKey || ev.ctrlKey || ev.metaKey) return;
    this.send(String.fromCharCode(code));
    this.keyPressHandled = true;
  }

  private input(ev: InputEv): void {
    if (ev.propagationStopped) return;
    if (ev.inputType === "insertText" && ev.data && !this.keyDownSeen && !this.keyPressHandled) {
      this.send(ev.data);
    }
  }

  private keyup(ev: KeyEv): void {
    this.keyDownSeen = false;
    this.keyPressHandled = false;
    void ev;
  }
}

/** PTY 바이트열에 DEL/CR을 적용해 셸 라인을 복원한다(제어열은 무시). */
function shellLines(stream: string): string[] {
  const lines: string[] = [];
  let line: string[] = [];
  const chars = Array.from(stream);
  for (let i = 0; i < chars.length; i++) {
    const ch = chars[i];
    if (ch === DEL) line.pop();
    else if (ch === "\r") {
      lines.push(line.join(""));
      line = [];
    } else if (ch === "\x03") {
      line = []; // Ctrl+C: 셸이 현재 줄을 버리고 새 프롬프트를 낸다
    } else if (ch === ESC) {
      // CSI 시퀀스(ESC [ … 최종 바이트)는 건너뛴다.
      if (chars[i + 1] === "[") {
        i += 2;
        while (i < chars.length && !/[A-Za-z~]/.test(chars[i])) i++;
      }
    } else if (ch < " ") {
      // 그 밖의 제어문자(Tab·EOT 등)는 셸이 해석한다 — 텍스트가 아니다.
    } else line.push(ch);
  }
  if (line.length > 0) lines.push(line.join(""));
  return lines;
}

function replayFixture(name: string): { pty: string; events: number; textarea: { value: string } } {
  const fixture = readFileSync(new URL(`./__fixtures__/${name}`, import.meta.url), "utf8");
  const textarea = { value: "" };
  const host = new FakeHost(textarea);
  let pty = "";
  const triggerDataEvent = vi.fn((data: string) => {
    pty += data;
  });
  const compositionView = {
    textContent: "",
    style: { letterSpacing: "" },
    classList: { add: vi.fn(), remove: vi.fn() },
  };
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
  new FakeXterm(host, textarea, (data) => triggerDataEvent(data));

  let now = 0;
  let events = 0;
  // CRLF 체크아웃(Windows autocrlf)에서도 마지막 필드 비교(`=== "1"`)가
  // 정확하도록 줄 끝 CR을 허용한다. .gitattributes가 tsv를 LF로 고정하지만
  // 이미 클론된 체크아웃까지 강제할 수는 없다.
  for (const line of fixture.split("\n")) {
    if (!line || line === "\r") continue;
    const f = line.replace(/\r$/, "").split("\t");
    const ts = Number(f[0]);
    if (ts > now) {
      vi.advanceTimersByTime(ts - now);
      now = ts;
    }
    events++;
    switch (f[1]) {
      case "kd": {
        const mod = f[5] === "1";
        host.dispatch(
          "keydown",
          stoppable({
            keyCode: Number(f[2]),
            key: JSON.parse(f[3]) as string,
            isComposing: false,
            shiftKey: f[4] === "1",
            ctrlKey: mod,
            altKey: false,
            metaKey: false,
          }),
        );
        break;
      }
      case "ku":
        host.dispatch("keyup", stoppable({ keyCode: Number(f[2]), key: f[3] ? (JSON.parse(f[3]) as string) : "" }));
        break;
      case "kp":
        host.dispatch("keypress", stoppable({ keyCode: Number(f[2]), key: f[3] ? (JSON.parse(f[3]) as string) : "" }));
        break;
      case "bi":
        host.dispatch("beforeinput", stoppable({ inputType: f[2], data: JSON.parse(f[3]) as string | null }));
        break;
      case "inp": {
        const deletes = Number(f[4]);
        const insert = JSON.parse(f[5]) as string;
        const chars = Array.from(textarea.value);
        textarea.value = chars.slice(0, chars.length - deletes).join("") + insert;
        host.dispatch("input", stoppable({ inputType: f[2], data: JSON.parse(f[3]) as string | null }));
        break;
      }
      default:
        throw new Error(`unknown fixture event: ${line}`);
    }
  }
  // Enter 뒤 resync(setTimeout 0)·홀드/백스톱 타이머까지 흘린다.
  vi.advanceTimersByTime(3000);
  detach();
  return { pty, events, textarea };
}

describe("attachWebKitImeBridge — 실기 WebKit 트레이스 리플레이", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("실사용 세션(한글 문장·마침표·Backspace 수정·스페이스·한/영 전환)이 PTY에 그대로 도달한다", () => {
    const { pty, events } = replayFixture("webkit-ime-trace-hangul-sentence.tsv");
    expect(events).toBeGreaterThan(1500);
    expect(pty).not.toContain(" "); // NBSP는 일반 공백으로 정규화된다
    expect(pty).not.toContain("\n");
    expect(shellLines(pty)).toEqual([
      "아직도 한글 입력에 대해서 이슈가 존재함. 전체적으로 다시 한 번 검토해서 한글과 영어 한글과 한글 그리고 " +
        "스페이스나 특수 문자들이 명확하게 입력하는 것이 그대로 반영되는지 그리고 그 시그널이 들어가는지 전체 검토해봐 " +
        "이슈가 있으면 설계문서 만들어서 진행해. 나에게 묻지말고 자율적으로 끝까지 수행",
    ]);
    // Enter는 정확히 한 번(CR 이중 전송 없음).
    expect(pty.split("\r").length - 1).toBe(1);
  });

  it("앱 시작 직후 IME가 우회된 자모 keydown 열은 오토마타가 음절로 조립한다(안녕하세요)", () => {
    // 실기: 앱 시작 후 첫 입력 — WebKit이 한글 입력 소스의 키를 IME에 넘기지 않고
    // `kd 68 key="ㅇ"`처럼 레이아웃 문자를 그대로 흘렸다. 종전에는 xterm이
    // "ㅇㅏㄴㄴㅕㅇㅎㅏㅅㅔㅇㅛ"를 그대로 보냈다.
    const { pty } = replayFixture("webkit-ime-trace-launch-bypass.tsv");
    expect(shellLines(pty)).toEqual(["안녕하세요"]);
    expect(pty.split("\r").length - 1).toBe(1);
  });

  it("새 브리지가 부착된 실사용 세션(CapsLock 홀드 창·Shift 자모·IME 조합·명령어)이 그대로 도달한다", () => {
    // 2026-09-13 실기: 부착 직후 홀드 창 4개(터미널 4개) → CapsLock(hold=1500) →
    // `kd 0 Unidentified` → blur → IME 조합(계속) → Enter, 이어서 영문 명령·/goal.
    // 마지막 줄의 "/goal "는 커서 이동(Home/방향키) 뒤 입력이라 스트림 복원상 끝에 붙는다.
    const { pty } = replayFixture("webkit-ime-trace-capslock-hold-session.tsv");
    expect(pty).not.toMatch(/[ㄱ-ㅣ]/); // 자모 유출 없음
    expect(shellLines(pty)).toEqual(["codex", "/model", "/model", "", "계속", "남은것도 끝내 /goal "]);
  });

  it("우회 자모 → 라틴 keydown(늦은 IME 삽입) → IME 조합으로 넘어가는 시작 경주에서 라틴이 새지 않는다", () => {
    // 실기: CapsLock 뒤 ㅇ·ㅏ는 우회, 세 번째 키는 `kd 83 key="s"`(라틴)로 왔고
    // keyup은 "ㄴ", 200ms 뒤 IME가 insertText "ㄴ"을 넣었다(IME가 두 번째 ㄴ은
    // 삼켰다). 종전 결과는 "ㅇㅏs녕하세요". 오토마타(아) + IME(녕하세요)로
    // 라틴 유출·자모 유출 없이 조립되어야 한다 — 경계의 음절 결합(안)은 IME가
    // 삼킨 키 때문에 이 트레이스로는 복원할 수 없다.
    const { pty } = replayFixture("webkit-ime-trace-launch-mixed.tsv");
    expect(pty).not.toMatch(/[a-z]/);
    expect(shellLines(pty)).toEqual(["아녕하세요"]);
  });
});
