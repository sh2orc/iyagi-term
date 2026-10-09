/**
 * Real xterm wiring (browser only — never imported by node unit tests).
 *
 * - fontSize/테마는 사용자 설정(preferences 스토어)에서 읽는다:
 *   dark/light 두 가지(04-ui §4), 기본 글자 크기 저장(W1-6).
 * - unicode11 addon으로 자소·이모지 폭을 Unicode 11 baseline에 맞춘다(W1-7).
 * - OSC 52 clipboard write는 기본 거절(사용자 설정으로만 허용 — 04-ui §6).
 * - OSC 0/2(동적 제목)·OSC 7(cwd)는 registerTerminalReporting으로
 *   세션 컨트롤러에 보고한다(W1-3/4).
 * - URL 인식은 https/http만, 사용자 클릭으로 OS opener 사용.
 * - 검색은 현재 scrollback(addon-search).
 * - WebGL 렌더러(addon-webgl)는 보이는 pane에만 붙인다(createWebglRenderer):
 *   박스·블록 글자를 셀 전체에 그려 lineHeight 1.1의 행 틈에서도 TUI
 *   테두리가 이어진다. 없거나 컨텍스트를 잃으면 DOM 렌더러로 물러난다.
 */

import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { SerializeAddon } from "@xterm/addon-serialize";
import { Unicode11Addon } from "@xterm/addon-unicode11";
// 패치된 WebGL 애드온(vendor/xterm-addon-webgl — 아틀라스 병합·공유 시 글리프 깨짐 수정). 업스트림 0.19.0은
// 페이지 병합이 그리기 도중 일어나면 그 프레임을 낡은 좌표로 그려 일부 글자가 다른 글리프 조각으로 보인다.
import { WebglAddon } from "../../vendor/xterm-addon-webgl/addon-webgl";
import type { IBufferLine, ILink, ILinkProvider, Terminal as ITerminal } from "@xterm/xterm";
import type { FitAddonFactory, FitAddonLike, RegistryDeps, TerminalFactory, TerminalLike } from "./registry";
import { DEFAULT_FONT_SIZE } from "./zoom";
import { lineHeightForFont, resolveDefaultFontFamily } from "./defaultFont";
import { WebKitImeCore, detectImeEngine, type ImeAction, type ImeEngine } from "./imeBridge";
import { currentPlatform, type Platform } from "./shortcuts";
import { defaultHangulToggle, isHangulToggleChord, type HangulToggle } from "./hangulToggle";
import { findUrls, openExternal } from "../security/linkOpener";
import { setHoveredLink } from "./linkHover";
import { base64ToBytes } from "../daemon/base64";
import { usePreferences } from "../../store/preferences";
import { resolveTheme, systemPrefersLight } from "../../app/theme";
import { cwdFromOsc7, sanitizeOscTitle } from "./osc";

// 팔레트 단일 원본은 terminalPalette.ts(node 시험 가능한 순수 모듈).
// 여기서 재수출해 기존 import 경로(Workbench 등)를 그대로 유지한다.
export { DARK_THEME, LIGHT_THEME, terminalTheme, type TerminalPalette, type TerminalThemeName } from "./terminalPalette";
import { terminalTheme } from "./terminalPalette";

/** User-settable(기본 거절이 계약 — 04 §6). 설정의 OSC 52 토글과 연결된다. */
let osc52Enabled = false;
export function setOsc52Enabled(enabled: boolean): void {
  osc52Enabled = enabled;
}

/** OSC 52로 클립보드에 쓸 수 있는 최대 바이트 — paste 상한과 대칭(W2). */
const OSC52_MAX_TEXT_BYTES = 65536;

/** scrollback 검색 접근(04 §6 R1: 현재 scrollback 검색, 다음/이전 이동). */
const searchAddons = new WeakMap<object, SearchAddon>();

export function getSearchAddon(terminal: unknown): SearchAddon | null {
  if (terminal && typeof terminal === "object") {
    return searchAddons.get(terminal) ?? null;
  }
  return null;
}

/** 화면 스냅샷 직렬화(replaySnapshot.ts — 앱 재시작 때 저널 전체 재생을 건너뛴다). */
const serializeAddons = new WeakMap<object, SerializeAddon>();

export interface SerializedTerminalState {
  data: string;
  cols: number;
  rows: number;
}

/**
 * 화면·스크롤백·대체 화면·모드를 되살리는 제어 시퀀스로 직렬화한다. SerializeAddon이
 * 빠뜨리는 상태 중 입력 해석에 영향을 주는 둘 — 마우스 보고 인코딩(SGR)과 커서
 * 숨김 — 을 xterm 내부에서 읽어 덧붙인다(버전별 내부 접근이라 없으면 건너뛴다).
 * 둘 다 커서를 옮기지 않으므로 직렬화 결과의 마지막 커서 위치를 해치지 않는다.
 */
export function serializeTerminalState(terminal: unknown): SerializedTerminalState | null {
  if (!terminal || typeof terminal !== "object") return null;
  const addon = serializeAddons.get(terminal);
  const term = terminal as ITerminal;
  if (!addon || !(term.cols > 0) || !(term.rows > 0)) return null;
  const core = (terminal as {
    _core?: {
      coreMouseService?: { activeEncoding?: string };
      coreService?: { isCursorHidden?: boolean };
      _inputHandler?: {
        _parser?: { currentState?: number };
        _utf8Decoder?: { interim?: Uint8Array };
      };
    };
  })._core;
  // PTY 청크 경계가 제어 시퀀스나 UTF-8 글자 한가운데일 수 있다. 파서가 그 중간이면
  // 직렬화로는 그 상태를 옮길 수 없어, 뒤이은 레코드가 깨진 글자로 시작한다 — 뜨지 않는다.
  const input = core?._inputHandler;
  if (input?._parser?.currentState !== undefined && input._parser.currentState !== 0) return null;
  if (input?._utf8Decoder?.interim?.[0]) return null;
  let data = addon.serialize();
  if (term.modes.mouseTrackingMode !== "none") {
    const encoding = core?.coreMouseService?.activeEncoding;
    if (encoding === "SGR") data += "\x1b[?1006h";
    else if (encoding === "SGR_PIXELS") data += "\x1b[?1016h";
  }
  if (core?.coreService?.isCursorHidden === true) data += "\x1b[?25l";
  return { data, cols: term.cols, rows: term.rows };
}

export interface TerminalFactoryOptions {
  /**
   * Return false to make xterm ignore the key (app combo — design §10:
   * 해당 조합을 PTY 입력으로 전달하지 않는다). Defaults to "xterm keeps it".
   */
  consumeKey?: (event: KeyboardEvent) => boolean;
  /** 실행 플랫폼(기본: UA 판정) — Windows면 ConPTY 힌트를 xterm에 준다. */
  platform?: Platform;
}

/**
 * xterm의 `windowsPty` 옵션: 데몬이 Windows에서 ConPTY로 세션을 여니
 * xterm에 알려 줘야 ConPTY가 접은 줄을 표시해 선택/복사에 개행이 끼지
 * 않고 resize 후 커서 동기화가 맞는다(옛 `windowsMode`의 역할).
 * 빌드 번호는 모르므로 생략한다.
 */
export function windowsPtyOption(platform: Platform): { backend: "conpty" } | undefined {
  return platform === "windows" ? { backend: "conpty" } : undefined;
}

/**
 * xterm 6은 Shift+Enter의 Shift를 무시하고 일반 Enter처럼 CR을 보낸다.
 * 대화형 CLI(예: Codex — 줄바꿈이 Ctrl+J = LF)에서 여러 줄 입력이 되도록
 * plain Shift+Enter를 다룬다:
 *  - keydown → "send-lf": LF(\n = Ctrl+J = 0x0a)를 한 번 보낸다.
 *  - keypress → "suppress": 삼킨다. 안 그러면 브라우저가 Enter에 대해 keypress도
 *    발생시키고 xterm의 _keyPress가 뒤이어 CR(\r)을 보내, PTY에 \n\r이 흘러가
 *    Codex 등에서 제출되거나 "줄만 커지는" 이상 동작이 된다.
 * 조합 중(isComposing)에는 xterm이 IME를 먼저 확정해야 하므로 관여하지 않는다.
 */
export type ShiftEnterAction = "send-lf" | "suppress" | null;
export function shiftEnterAction(
  event: Pick<
    KeyboardEvent,
    "type" | "key" | "keyCode" | "shiftKey" | "ctrlKey" | "altKey" | "metaKey" | "isComposing"
  >,
): ShiftEnterAction {
  const isShiftEnter =
    (event.key === "Enter" || event.keyCode === 13) &&
    event.shiftKey &&
    !event.ctrlKey &&
    !event.altKey &&
    !event.metaKey &&
    !event.isComposing;
  if (!isShiftEnter) return null;
  if (event.type === "keydown") return "send-lf";
  if (event.type === "keypress") return "suppress";
  return null;
}

export function createDefaultTerminalFactory(options: TerminalFactoryOptions = {}): TerminalFactory {
  return () => {
    const prefs = usePreferences.getState();
    const windowsPty = windowsPtyOption(options.platform ?? currentPlatform());
    const fontFamily = prefs.fontFamily ?? resolveDefaultFontFamily();
    const term = new Terminal({
      ...(windowsPty ? { windowsPty } : {}),
      fontSize: prefs.baseFontSize ?? DEFAULT_FONT_SIZE,
      // 글자 높이가 낮은 글꼴(내장 Nanum Gothic Coding 등)도 행 높이가 같게.
      lineHeight: lineHeightForFont(fontFamily),
      // 문장부호(따옴표·화살표·em 대시)의 좌우 여백이 좁아 이웃 글자와
      // 붙어 보이는 것을 완화한다.
      letterSpacing: 0.5,
      scrollback: prefs.scrollbackLines ?? 2000,
      fontFamily,
      cursorStyle: prefs.cursorStyle ?? "block",
      cursorBlink: true,
      // 박스·블록·파워라인 글자를 글꼴 대신 셀 크기로 직접 그린다 — WebGL
      // 렌더러에서만 효력이 있고 DOM 렌더러는 글꼴 그대로 그린다(xterm 계약).
      customGlyphs: true,
      allowProposedApi: true,
      theme: terminalTheme(resolveTheme(prefs.theme, systemPrefersLight())),
    });
    // OSC 8 하이퍼링크(xterm 6 네이티치트): 활성화는 우리 링크 정책
    // (http/https만, OS opener)을 따른다 — confirm 폴백의 무조건 경고 대신.
    term.options.linkHandler = {
      activate: (_event, text) => {
        if (/^https?:\/\//i.test(text)) void openExternal(text);
      },
      // 오른쪽 클릭 메뉴의 링크 항목 근거 — 여는 정책과 같게 http/https만.
      hover: (_event, text) => setHoveredLink(term, /^https?:\/\//i.test(text) ? text : null),
      leave: () => setHoveredLink(term, null),
    };
    const search = new SearchAddon();
    term.loadAddon(search);
    searchAddons.set(term, search);
    const serialize = new SerializeAddon();
    term.loadAddon(serialize);
    serializeAddons.set(term, serialize);

    // Unicode 11 width tables — 한글 자소 조합·이모지 폭 정확도(W1-7).
    const unicode11 = new Unicode11Addon();
    term.loadAddon(unicode11);
    try {
      term.unicode.activeVersion = "11";
    } catch {
      // 활성화 실패 시 기본 unicode 버전으로 동작한다(치명적 아님).
    }

    // Shift+Space 한/영 강제 전환(hangulToggle.ts). 전역 인스턴스는 처음 만들 때 OS
    // 가용성(ime_state)을 조회하므로 터미널 생성 시점에 데운다 — "자동" 모드의 첫
    // Shift+Space가 판정 전이라 공백으로 새지 않게.
    const hangulToggle = defaultHangulToggle();
    // 앱 단축키 조합은 xterm이 소비하지 않게 한다(전역 handler가 처리).
    term.attachCustomKeyEventHandler((event) => {
      // 켜져 있으면 PTY에 공백을 보내지 않고 OS 입력기를 바꾼다. WebKit(macOS)에서는
      // IME 브리지의 capture 리스너가 Space를 xterm보다 먼저 가지므로 거기서 먼저
      // 판정한다(attachWebKitImeBridge) — 이 경로는 브리지가 없는 엔진(WebView2 등)과
      // 네이티브 조합 구간을 맡는다.
      if (hangulToggle.handleKeydown(event, term)) return false;
      const shiftEnter = shiftEnterAction(event);
      if (shiftEnter === "send-lf") {
        term.input("\n", true);
        return false; // xterm의 CR 전송 차단
      }
      if (shiftEnter === "suppress") {
        return false; // 뒤따르는 keypress를 삼켜 CR(\r) 이중 전송을 막는다
      }
      return options.consumeKey ? options.consumeKey(event) : true;
    });

    // OSC 52: 기본 거절. 활성화 시에만, 상한 안의 텍스트만 clipboard에 쓴다.
    term.parser.registerOscHandler(52, (data: string) => {
      if (!osc52Enabled) return true; // swallow = deny
      const base64 = data.split(";").pop() ?? "";
      try {
        const text = new TextDecoder().decode(base64ToBytes(base64));
        if (text.length > OSC52_MAX_TEXT_BYTES) return true; // 상한 초과는 거절
        void navigator.clipboard?.writeText(text).catch(() => undefined);
      } catch {
        // 잘못된 인코딩은 무시한다.
      }
      return true;
    });

    term.registerLinkProvider(makeUrlLinkProvider(term));
    return term as unknown as TerminalLike;
  };
}

export const createDefaultFitAddon: FitAddonFactory = () => new FitAddon() as unknown as FitAddonLike;

/** xterm 내부 표면 — IME 조정기가 쓰는 최소 부분만(없으면 no-op). */
interface XtermInternals {
  coreService?: { triggerDataEvent?(data: string, wasUserInput?: boolean): void };
  _keyDownSeen?: boolean;
  _compositionHelper?: {
    _isComposing: boolean;
    _compositionView: HTMLElement;
    updateCompositionElements(dontRecurse?: boolean): void;
  };
}

/**
 * WebKit(WKWebView) IME 조정기 부착 — 상세 계약은 imeBridge.ts와
 * docs/implementation/07-korean-ime.md 참조.
 * host(xterm이 open된 컨테이너)의 capture 단계에 붙어 대상(textarea)의
 * xterm 리스너보다 먼저 돈다: insertReplacementText(조합)와 수정키 없는
 * Space는 우리가, 나머지 insertText는 게이트 리셋만 보조해 xterm이 맡고,
 * 229 keydown(활성 후)은 차단한다. compositionstart만 오면 폴백을 유지하고
 * (영→한 전환 직후 단독 발화), compositionupdate~end 구간에는 물러나
 * xterm 네이티브 조합기에 전권을 넘긴다. 다음 입력이 다시 WebKit 폴백
 * 이벤트로 오면 브리지가 재활성화된다.
 * 내부 API(_core)에 접근할 수 없으면 조용히 no-op이다.
 *
 * 조합 보류 중에는 xterm의 composition view(인라인 preedit)를 우리가
 * 구동한다 — 조합 이벤트가 없어도 커서 위치에 밑줄 텍스트로 즉시
 * 피드백을 주고, 셸은 확정 시점에만 갱신된다(네이티브 터미널과 동일).
 *
 * WebKit 전용이다(imeBridge.ts detectImeEngine): Chromium 계열(WebView2·
 * Chrome·Edge)에서는 아무 리스너도 붙이지 않고 xterm 네이티브 조합기에
 * 맡긴다 — CapsLock 한/영 풍선도 함께 꺼진다(Windows는 CapsLock이 대소문자
 * 전환이고 한/영은 별도 키 `HangulMode`다). `options.engine`은 시험 주입용.
 *
 * Shift+Space 한/영 강제 전환(hangulToggle.ts)도 이 capture 리스너가 코어보다
 * 먼저 판정한다 — 수정키 없는 Space를 xterm보다 먼저 차단하는 곳이 여기라서다.
 * `options.hangulToggle`은 시험 주입용(기본: 앱 전역 인스턴스).
 */
export function attachWebKitImeBridge(
  host: HTMLElement,
  terminal: unknown,
  options: { engine?: ImeEngine; hangulToggle?: Pick<HangulToggle, "handleKeydown" | "availability"> } = {},
): () => void {
  if ((options.engine ?? detectImeEngine()) !== "webkit") return () => undefined;
  const internals = (terminal as { _core?: XtermInternals })._core;
  const trigger = internals?.coreService?.triggerDataEvent?.bind(internals.coreService);
  const textarea = host.querySelector<HTMLTextAreaElement>("textarea");
  if (!internals || !trigger || !textarea) return () => undefined;
  const core = new WebKitImeCore();
  const hangulToggle = options.hangulToggle ?? defaultHangulToggle();
  let settleTimer: ReturnType<typeof setTimeout> | null = null;
  let holdTimer: ReturnType<typeof setTimeout> | null = null;
  // IME 진단 로그 — 마지막 400개 이벤트를 window.__imeEvents로 노출한다.
  // 재현 보고가 있을 때 웹 인스펙터에서 읽어 실 이벤트 순서를 확정한다.
  const imeEvents: string[] = [];
  const imeLogView = host.ownerDocument?.defaultView ??
    (typeof window === "undefined" ? undefined : window);
  if (imeLogView) {
    (imeLogView as unknown as { __imeEvents?: string[] }).__imeEvents = imeEvents;
  }
  // 개발 서버(vite HMR)가 붙어 있으면 트레이스를 파일로도 흘려보낸다 —
  // vite.config.ts의 iyagi-ime-trace 플러그인이 받는다(프로덕션엔 hot 없음).
  const hot = import.meta.hot;
  let traceQueue: string[] = [];
  let traceTimer: ReturnType<typeof setTimeout> | null = null;
  const note = (message: string): void => {
    const line = `${Math.round(performance.now())} ${message}`;
    imeEvents.push(line);
    if (imeEvents.length > 400) imeEvents.shift();
    if (!hot) return;
    traceQueue.push(line);
    if (traceTimer !== null) return;
    traceTimer = setTimeout(() => {
      traceTimer = null;
      const lines = traceQueue;
      traceQueue = [];
      hot.send("iyagi:ime-trace", { lines });
    }, 250);
  };
  let renderedPreedit = "";
  // 표준 compositionupdate~compositionend 구간에는 xterm 네이티브 조합기가
  // textarea와 composition view를 단독 소유한다. WebKit은 같은 attach에서도
  // 다음 입력부터 폴백 이벤트로 돌아갈 수 있으므로 영구 전환하지 않는다.
  let nativeCompositionActive = false;
  // 폴백 폰트의 한글 어드밴스는 2셀 격자보다 좁다(예: 0.865em vs 1.2em).
  // preet를 격자 폭에 맞춰 늘려야 확정 시 글자가 오른쪽으로 점프하지
  // 않는다. 폰트/크기별로 한 번만 측정한다.
  let spacingCache: { fontSize: number; spacing: string } | null = null;
  const hangulLetterSpacing = (): string => {
    const cell = (internals as {
      _renderService?: { dimensions?: { css?: { cell?: { width: number } } } };
    })._renderService?.dimensions?.css?.cell;
    const options = (terminal as { options?: { fontFamily?: string; fontSize?: number } }).options;
    if (!cell || !options?.fontFamily) return "";
    if (spacingCache && spacingCache.fontSize === options.fontSize) return spacingCache.spacing;
    let spacing = "";
    try {
      const ctx = document.createElement("canvas").getContext("2d");
      if (ctx) {
        ctx.font = `${options.fontSize ?? 13}px ${options.fontFamily}`;
        const advance = ctx.measureText("한").width;
        const want = cell.width * 2;
        if (advance > 0 && want > advance) spacing = `${want - advance}px`;
      }
    } catch {
      // 측정 실패 시 격자 보정 없이 동작한다.
    }
    spacingCache = { fontSize: options.fontSize ?? 0, spacing };
    return spacing;
  };
  // 조합 중 .xterm 루트에 .ime-composing을 부여/해제한다 — CSS가 그때만
  // 블록 캐럿을 감춰 투명 preedit 뒤로 커서가 비치지 않게 한다. (테스트의
  // FakeHost처럼 classList가 없으면 조용히 no-op.)
  const setComposingClass = (on: boolean): void => {
    const root = host.querySelector<HTMLElement>(".xterm");
    const cl = (root as { classList?: DOMTokenList } | null)?.classList;
    if (cl) cl.toggle("ime-composing", on);
  };
  const renderPreedit = (): void => {
    if (nativeCompositionActive) return;
    const helper = internals._compositionHelper;
    if (!helper?._compositionView) return;
    const text = core.pendingText;
    if (text === renderedPreedit) return;
    renderedPreedit = text;
    helper._isComposing = text.length > 0;
    helper._compositionView.textContent = text;
    if (text.length > 0) {
      helper._compositionView.style.letterSpacing = hangulLetterSpacing();
      helper._compositionView.classList.add("active");
      setComposingClass(true);
      helper.updateCompositionElements();
    } else {
      helper._compositionView.classList.remove("active");
      setComposingClass(false);
    }
  };
  // 확정된 조합 문자열을 hidden textarea에서 비운다 — 남겨두면 셸에서
  // Backspace/Alt+Backspace로 지운 과거 문자열이 다음 입력의 diff에 통째로
  // 잡혀 재전송된다. 다만 WebKit 폴백 IME는 확정 마커(값 불변 replacement)
  // 뒤에도 같은 textarea에 다음 음절을 계속 쌓는다 — 그 시점에 비우면
  // 진행 중 조합이 끊겨 자모가 샌다(안녕하세요 → 안ㄴㅕㅇ하세요). 그래서
  // 마커에서 즉시 비우지 않고 "예약(arm)"만 하고, 다음 이벤트가 조합
  // continuation이면 취소, 확정/실키/경계/유휴면 그때 실제로 비운다.
  // 확정된 조합 문자열을 hidden textarea에서 비운다 — 남겨두면 셸에서
  // 단어/줄 삭제(Alt/Cmd+Backspace, Ctrl+U/W)로 지운 과거 문자열이 다음
  // 입력의 diff에 통째로 잡혀 재전송된다. 조합 중(pendingText)이거나 이미
  // 빈 경우엔 건드리지 않는다. 반드시 apply(=emit) '이후'에만 호출한다 —
  // apply 전에 비우면 이번 입력이 담긴 textarea.value까지 지워져 스페이스·
  // 영문이 유실된다(스페이스 먹힘의 원인이었다).
  // NOTE: 예전에 확정 후 textarea.value를 ""로 비우던 clearSettledTextArea가
  // 있었으나 제거했다. WebKit IME는 확정 뒤에도 조합을 내부적으로 들고 있어,
  // textarea를 프로그램적으로 비우면 다음 Backspace에서 조합을 복원해 엉뚱한
  // 자모를 만들어냈다(안→안아, 하→하ㅎ, 스페이스 전체 유실). \x7f는 Codex
  // 등에서 정상 동작하므로, 비우지 않고 꼬리 diff(DEL+삽입)에 맡기면 분해·
  // 삭제가 깔끔하다. 줄 리셋은 CR/ETX/ESC의 resync에서만 한다(아래).
  const armSettle = (): void => {
    if (settleTimer !== null) clearTimeout(settleTimer);
    settleTimer = setTimeout(() => {
      settleTimer = null;
      note("settle-timer");
      apply(core.settle());
      renderPreedit();
      // NOTE: 여기서 textarea를 비우지 않는다. 2초 유휴 후 비우면 WebKit이
      // 아직 들고 있는 조합과 어긋나(shellValue/textarea 데스싱크) 이후
      // 스페이스·입력에서 대량 DEL/중복이 나올 수 있다. 확정 후 조합 리셋은
      // 커서 이동 키(방향키 등)에서만 한다(사용자의 명시적 "이동" 시점).
    }, core.settleDelayMs);
  };
  // 입력 소스 전환 홀드 창의 타이머 — 코어가 holdTimerMs로 (재)가동을 요청한다.
  // 만료되면 보류 키를 판정해 내보낸다(imeBridge.ts releaseHeldLatin).
  const armHold = (ms: number): void => {
    if (holdTimer !== null) clearTimeout(holdTimer);
    holdTimer = setTimeout(() => {
      holdTimer = null;
      note("hold-timer");
      apply(core.releaseHeldLatin());
    }, ms);
  };
  const apply = (action: ImeAction, ev?: Event): void => {
    if (action.emit || action.block || action.preventDefault || action.resetKeyDownSeen || action.holdTimerMs) {
      note(
        `  -> ${action.emit !== undefined ? `emit=${JSON.stringify(action.emit)} ` : ""}${action.block ? "block " : ""}${action.preventDefault ? "preventDefault " : ""}${action.resetKeyDownSeen ? "resetKeyDownSeen " : ""}${action.scheduleSettle ? "scheduleSettle " : ""}${action.holdTimerMs ? `hold=${action.holdTimerMs}` : ""}`,
      );
    }
    if (action.resetKeyDownSeen) internals._keyDownSeen = false;
    if (action.emit) trigger(action.emit, true);
    if (action.block && ev) ev.stopPropagation();
    if (action.preventDefault && ev) ev.preventDefault();
    if (action.scheduleSettle) armSettle();
    if (action.holdTimerMs) armHold(action.holdTimerMs);
    renderPreedit();
    // textarea 비우기는 이벤트 핸들러(onInput/onKeyDown/resync/settle-timer)가
    // 조합 continuation 여부를 보고 예약·취소·실행한다 — 여기서 즉시 비우면
    // 확정 마커 뒤에도 이어지는 조합이 끊긴다(자모 유출).
  };
  const onBeforeInput = (ev: Event): void => {
    const input = ev as InputEvent;
    note(`bi ${input.inputType} data=${JSON.stringify(input.data ?? null)}`);
    if (nativeCompositionActive || input.inputType.toLowerCase().includes("composition")) return;
    apply(core.beforeinput(input.inputType, textarea.value));
  };
  const onInput = (ev: Event): void => {
    const input = ev as InputEvent;
    note(
      `inp ${input.inputType} data=${JSON.stringify(input.data ?? null)} value=${JSON.stringify(textarea.value)}`,
    );
    if (nativeCompositionActive || input.inputType.toLowerCase().includes("composition")) return;
    apply(core.input(input.inputType, textarea.value, input.data ?? undefined), ev);
  };
  const onKeyDown = (ev: KeyboardEvent): void => {
    const key = typeof ev.key === "string" ? ev.key : undefined;
    note(
      `kd ${ev.keyCode} key=${JSON.stringify(ev.key)} composing=${ev.isComposing} shift=${ev.shiftKey} mod=${ev.ctrlKey || ev.altKey || ev.metaKey}`,
    );
    if (nativeCompositionActive) return;
    // Shift+Space 한/영 강제 전환(hangulToggle.ts)은 코어보다 먼저 판정한다 — 이
    // capture 리스너는 수정키 없는 Space를 xterm보다 먼저 차단하므로(Space 단일
    // 소유, Shift는 수정키로 치지 않는다) xterm custom key handler에 둔 전환은
    // WebKit에서 영영 불리지 않았다(실기 트레이스: `kd 32 shift=true -> block` 뒤
    // 공백 전송). 소비되면 토글이 preventDefault로 공백 삽입·keypress를 막았고,
    // 코어가 보류 키·조합을 확정한 뒤 전환 직후 첫 키들을 홀드 창으로 가른다.
    const toggled = hangulToggle.handleKeydown(ev, terminal, (state) => {
      note(`hangul-toggle ${JSON.stringify(state)}`);
      // CapsLock 풍선의 방향 추적을 OS가 알려 준 실제 상태로 맞춘다.
      if (!("error" in state) && state.hangul !== null) imeMode = state.hangul ? "ko" : "en";
    });
    if (toggled) {
      note("hangul-toggle-chord");
      apply(core.inputSourceToggled());
      return;
    }
    if (isHangulToggleChord(ev)) {
      // 진단: 전환이 꺼져 있어(설정 off, 또는 자동인데 한국어 입력 소스 없음·미판정)
      // Shift+Space가 보통 Space로 흘렀다 — 트레이스만 보고 원인을 가르게 남긴다.
      note(
        `hangul-toggle-pass mode=${usePreferences.getState().hangulToggle} available=${String(hangulToggle.availability())}`,
      );
    }
    apply(core.keydown(ev.keyCode, ev.isComposing, ev.ctrlKey || ev.altKey || ev.metaKey, key), ev);
    if (ev.keyCode === 229 || isJamoKey(key)) {
      imeMode = "ko";
    } else if (ev.keyCode >= 65 && ev.keyCode <= 90 && key !== undefined && /^[A-Za-z]$/.test(key)) {
      // 알파벳 키가 229 없이 라틴 `key`로 오면 영문 모드다. 공백·숫자·
      // 기호는 한글 모드에서도 실제 keyCode로 오므로 판별에 쓰지 않는다.
      imeMode = "en";
    }
    if (ev.keyCode === 20) {
      // CapsLock = 한/영 전환. 풍선은 마지막으로 관찰된 모드의 반대로
      // (타이핑 관찰로 곧 보정된다). 전환 방향과 무관하게 입력 소스 홀드
      // 창을 연다 — 판정은 코어가 보류 키의 keyup(`key`가 라틴/자모)과 IME
      // 삽입으로 한다(docs/implementation/07-korean-ime.md §3-1).
      imeMode = imeMode === "ko" ? "en" : "ko";
      showModeBalloon(imeMode);
      apply(core.armInputSourceHold());
    } else if (ev.keyCode === 0 || key === "Unidentified") {
      // WebKit이 입력 소스 전환 직후 내는 잡음 keydown — 전환이 실제로
      // 일어났다는 신호이므로(메뉴바·단축키 전환 포함) 홀드 창을 (다시) 연다.
      apply(core.armInputSourceHold());
    }
    // 커서 이동 키(PgUp/PgDn/End/Home/방향키) — 셸의 커서는 옮겨졌지만 WebKit
    // 한글 IME는 마지막 음절을 계속 "조합 가능"으로 들고 있어 다음 Backspace가
    // 삭제가 아니라 자소 분해(안→아)로 와 엉뚱한 자리(새 커서 앞)를 지운다.
    // textarea.value를 비우면 IME 내부 상태와 어긋나므로(불변식 4) 값은 두고
    // 선택 범위만 0→끝으로 흔든다 — WebKit은 선택이 바뀌면 입력 컨텍스트에
    // 마킹 폐기를 알려 IME가 새 음절부터 시작한다. 지원 안 되는 환경에서는
    // 아무 일도 일어나지 않는다(종전 동작 유지).
    if (ev.keyCode >= 33 && ev.keyCode <= 40 && !ev.ctrlKey && !ev.altKey && !ev.metaKey) {
      nudgeImeSelection();
    }
    // xterm이 CR/ETX/ESC로 textarea를 비울 때만 재동기화 — 매 keydown마다
    // 하면 보류 중 조합을 파괴한다.
    if (ev.keyCode === 13 || ev.keyCode === 27 || (ev.ctrlKey && ev.keyCode === 67)) {
      setTimeout(() => {
        note(`resync value=${JSON.stringify(textarea.value)}`);
        core.resync(textarea.value);
        renderPreedit();
      }, 0);
    }
  };
  const nudgeImeSelection = (): void => {
    const ta = textarea as { value: string; setSelectionRange?: (a: number, b: number) => void };
    if (typeof ta.setSelectionRange !== "function") return;
    try {
      const end = ta.value.length;
      ta.setSelectionRange(0, 0);
      ta.setSelectionRange(end, end);
      note("nudge-selection");
    } catch {
      // 선택 범위를 못 바꾸는 상태(비활성 등)면 무시한다.
    }
  };
  const onKeyUp = (ev: KeyboardEvent): void => {
    note(`ku ${ev.keyCode} key=${JSON.stringify(ev.key)}`);
    if (nativeCompositionActive) return;
    apply(core.keyup(ev.keyCode, typeof ev.key === "string" ? ev.key : undefined));
  };
  const isJamoKey = (key: string | undefined): boolean =>
    typeof key === "string" && key.length === 1 && key >= "ㄱ" && key <= "ㅣ";
  // compositionstart/end는 기록하고, update는 아래에서 네이티브 경로를
  // 확정하는 신호로도 사용한다.
  const onCompositionLog = (ev: Event): void => {
    note(`${ev.type} data=${JSON.stringify((ev as CompositionEvent).data ?? null)} value=${JSON.stringify(textarea.value)}`);
  };
  // xterm 6.0은 A–Z 대문자를 keydown이 아닌 keypress에서 보낸다(macOS IME
  // HACK). 자체 전송한 키·홀드 키의 keypress만 core가 판정해 차단한다.
  const onKeyPress = (ev: KeyboardEvent): void => {
    note(`kp ${ev.keyCode} key=${JSON.stringify(ev.key)}`);
    if (nativeCompositionActive) return;
    if (core.keypress(ev.keyCode).block) ev.stopPropagation();
  };

  // 한/영(CapsLock) 전환 풍선 — macOS의 시스템 HUD는 포커스된 네이티브
  // 입력의 캐럿 위치로 뜨는데, WKWebView의 숨은 textarea는 그 위치를
  // 제대로 노출하지 않아 우리 앱에는 오지 않는다. 앱이 직접 그린다:
  // 관찰된 입력 흐름(229=한글, 실제 라틴 키=영문)으로 마지막 모드를
  // 추적하고, CapsLock이 그 모드를 뒤집는다고 간주해 캐럿 옆에 띄운다.
  let imeMode: "ko" | "en" | null = null;
  const showModeBalloon = (mode: "ko" | "en"): void => {
    const helper = internals._compositionHelper;
    const parent = helper?._compositionView?.parentElement;
    const dims = (
      internals as { _renderService?: { dimensions?: { css?: { cell?: { width: number; height: number } } } } }
    )._renderService?.dimensions?.css;
    const buffer = (terminal as { buffer?: { active?: { cursorX: number; cursorY: number } } }).buffer?.active;
    if (!parent || !dims?.cell || !buffer) return;
    parent.querySelector(".ime-mode-balloon")?.remove();
    const balloon = document.createElement("div");
    balloon.className = `ime-mode-balloon ime-mode-${mode}`;
    balloon.textContent = mode === "ko" ? "한" : "영";
    balloon.style.left = `${Math.max(buffer.cursorX * dims.cell.width - 4, 0)}px`;
    balloon.style.top = `${buffer.cursorY * dims.cell.height}px`;
    balloon.addEventListener("animationend", () => balloon.remove());
    parent.appendChild(balloon);
  };
  const onCompositionStart = (ev: Event): void => {
    onCompositionLog(ev);
    if (nativeCompositionActive) return;
    // 보류분을 마저 확정한다(물러나지 않는다 — imeBridge.ts 참조).
    apply(core.compositionstart());
    renderedPreedit = "";
    if (settleTimer !== null) {
      clearTimeout(settleTimer);
      settleTimer = null;
    }
  };
  const onCompositionUpdate = (ev: Event): void => {
    onCompositionLog(ev);
    if (nativeCompositionActive) return;
    nativeCompositionActive = true;
    note("native-composition");
    renderedPreedit = "";
    if (settleTimer !== null) {
      clearTimeout(settleTimer);
      settleTimer = null;
    }
    if (holdTimer !== null) {
      clearTimeout(holdTimer);
      holdTimer = null;
    }
    // 현재 textarea는 이제 xterm 네이티브 조합기의 소유다. 폴백의 로컬
    // diff만 맞춰 두고 DOM이나 xterm 이벤트 상태에는 손대지 않는다.
    core.resync(textarea.value);
  };
  const onCompositionEnd = (ev: Event): void => {
    onCompositionLog(ev);
    if (!nativeCompositionActive) return;
    // compositionend 시점에는 textarea에 최종 문자열이 들어 있다. 다음
    // 입력이 폴백 형태로 바뀌어도 누적 접두사를 다시 보내지 않게 기준점을
    // 맞춘 뒤 폴백을 재활성화한다. DOM 값은 xterm의 지연 전송이 읽어야 한다.
    core.resync(textarea.value);
    renderedPreedit = "";
    nativeCompositionActive = false;
  };
  // xterm은 textarea blur에서 value를 비운다(_handleTextAreaBlur — 클릭·다른
  // 패널·창 전환). 코어가 모른 채 다음 입력을 옛 shellValue와 diff하면 그
  // 길이만큼 DEL이 나가 xterm이 보낸 라틴까지 지운다(ccd + 스페이스 → DEL×5).
  // blur는 capture라 xterm 리스너보다 먼저 온다: 보류 조합을 먼저 내보내고,
  // xterm이 비운 뒤(다음 태스크) 기준점을 재동기화한다. 코어의 beforeinput
  // 자가 복구(rebase)는 이 리스너가 못 본 비우기에 대한 안전망이다.
  const onBlur = (): void => {
    if (nativeCompositionActive) return;
    note("blur");
    apply(core.settle());
    if (settleTimer !== null) {
      clearTimeout(settleTimer);
      settleTimer = null;
    }
    setTimeout(() => {
      note(`resync(blur) value=${JSON.stringify(textarea.value)}`);
      core.resync(textarea.value);
      renderPreedit();
    }, 0);
  };
  // 앱 시작 직후에는 WebKit이 한글 입력 소스의 키를 IME에 넘기지 않는 구간이
  // 있다(실기: 자모 keydown 우회·라틴 keydown 뒤 늦은 IME 삽입). 부착 시점에
  // 홀드 창을 열어 첫 키들을 keyup 판정으로 가른다 — 영문이면 첫 키 누름
  // 시간만큼만 지연되고, 한글이면 자모로 복원된다.
  apply(core.armInputSourceHold());
  host.addEventListener("beforeinput", onBeforeInput, true);
  host.addEventListener("input", onInput, true);
  host.addEventListener("keydown", onKeyDown, true);
  host.addEventListener("keyup", onKeyUp, true);
  host.addEventListener("keypress", onKeyPress, true);
  host.addEventListener("compositionstart", onCompositionStart, true);
  host.addEventListener("compositionupdate", onCompositionUpdate, true);
  host.addEventListener("compositionend", onCompositionEnd, true);
  host.addEventListener("blur", onBlur, true);
  return () => {
    if (settleTimer !== null) clearTimeout(settleTimer);
    if (holdTimer !== null) clearTimeout(holdTimer);
    if (traceTimer !== null) {
      clearTimeout(traceTimer);
      traceTimer = null;
    }
    traceQueue = [];
    // 전역 진단 포인터가 이 터미널의 로그를 가리키면 끊는다 — 안 그러면
    // 폐기된 xterm의 클로저 묶음이 window를 통해 계속 살아 있다.
    const view = imeLogView as unknown as { __imeEvents?: string[] } | undefined;
    if (view && view.__imeEvents === imeEvents) delete view.__imeEvents;
    const helper = internals._compositionHelper;
    if (helper?._compositionView) {
      helper._isComposing = false;
      helper._compositionView.textContent = "";
      helper._compositionView.classList.remove("active");
    }
    setComposingClass(false); // 조합 중 캐럿 숨김 클래스 정리
    host.removeEventListener("beforeinput", onBeforeInput, true);
    host.removeEventListener("input", onInput, true);
    host.removeEventListener("keydown", onKeyDown, true);
    host.removeEventListener("keyup", onKeyUp, true);
    host.removeEventListener("keypress", onKeyPress, true);
    host.removeEventListener("compositionstart", onCompositionStart, true);
    host.removeEventListener("compositionupdate", onCompositionUpdate, true);
    host.removeEventListener("compositionend", onCompositionEnd, true);
    host.removeEventListener("blur", onBlur, true);
  };
}

/** OSC 0/2(동적 제목)·OSC 7(cwd)로 컨트롤러에 보고할 내용. */
export interface TerminalReportingHandlers {
  onTitle?(title: string): void;
  onCwd?(cwd: string): void;
}

/**
 * 동적 제목(OSC 0/2)과 cwd(OSC 7) 보고를 등록한다. attach마다 한 번
 * 호출하고, 저널 재생 중에 같은 시퀀스가 다시 와도 값 설정이라 멱등이다.
 */
export function registerTerminalReporting(terminal: unknown, handlers: TerminalReportingHandlers): void {
  const parser = (
    terminal as {
      parser?: { registerOscHandler(code: number, handler: (data: string) => boolean): void };
    }
  ).parser;
  if (!parser) return;
  const handleTitle = (data: string): boolean => {
    const title = sanitizeOscTitle(data);
    if (title) handlers.onTitle?.(title);
    return true; // swallow — 기본 동작 없음
  };
  parser.registerOscHandler(0, handleTitle);
  parser.registerOscHandler(2, handleTitle);
  parser.registerOscHandler(7, (data) => {
    const cwd = cwdFromOsc7(data);
    if (cwd) handlers.onCwd?.(cwd);
    return true;
  });
}

/**
 * WebGL 렌더러(xterm 6.0.0의 짝 애드온 0.19.0을 vendor/xterm-addon-webgl에서 패치해
 * 묶은 것 — 그 README 참조). DOM 렌더러는 모든 글자를
 * 글꼴로 그려 `lineHeight`의 행 틈에서 박스·블록 글자(TUI 테두리의
 * `┃`·`▀` 등)가 행마다 끊겨 보이지만, WebGL 렌더러는 `customGlyphs`로 그
 * 글자들을 셀 전체에 직접 그린다.
 *
 * WebGL2가 없으면 null(DOM 렌더러 유지). 컨텍스트를 잃으면(WebKit의 활성
 * 컨텍스트 상한·GPU 리셋) 애드온을 버려 그 pane만 DOM 렌더러로 돌아간다
 * — 이때 `hooks.onDead`로 레지스트리에 알려 다음 mount/syncRenderer가
 * WebGL을 다시 만들게 한다(래퍼가 살아 있는 채로 남으면 영영 DOM 렌더러에
 * 갇힌다). 레지스트리가 보이는 pane에만 붙이므로 동시 컨텍스트는 한 탭의
 * pane 수(≤ 8)로 유계다.
 */
export const createWebglRenderer: NonNullable<RegistryDeps["createRenderer"]> = (terminal, hooks) => {
  if (typeof document === "undefined" || typeof WebGL2RenderingContext === "undefined") return null;
  // Keep the last painted frame available for resize snapshots. Redrawing
  // the mutable buffer to recover a discarded canvas would expose unfinished
  // synchronized CLI output and produce an extra visual step during zoom.
  let addon: WebglAddon | null = new WebglAddon(true);
  let disposed = false;
  const dispose = (): void => {
    if (disposed) return;
    disposed = true;
    const current = addon;
    addon = null;
    try {
      current?.dispose();
    } catch {
      // 이미 잃은 컨텍스트의 정리 실패는 무시한다.
    }
    // 죽음은 정확히 한 번만 알린다(disposed 게이트가 멱등을 보장). registry가
    // 시작한 해제(dropRenderer)면 이미 자기 필드를 비웠으므로 다시 비우기만
    // 하는 무해한 호출이고, 컨텍스트 상실이면 래퍼를 비워 재생성을 연다.
    hooks.onDead?.();
  };
  addon.onContextLoss(() => dispose());
  try {
    terminal.loadAddon(addon);
  } catch {
    // "WebGL2 not supported" 등 — DOM 렌더러 그대로.
    dispose();
    return null;
  }
  return {
    dispose,
    // Rebuild glyph coordinates and atlas pixels together; refreshing rows alone
    // reuses cached GPU cells. The addon also requests a full viewport redraw.
    refresh: () => addon?.clearTextureAtlas(),
  };
};

/** Browser ResizeObserver wiring for the registry. */
export const browserResizeObserverFactory: NonNullable<RegistryDeps["createResizeObserver"]> = (callback) =>
  typeof ResizeObserver !== "undefined"
    ? new ResizeObserver(() => callback())
    : { observe: () => undefined, unobserve: () => undefined, disconnect: () => undefined };

function makeUrlLinkProvider(term: ITerminal): ILinkProvider {
  return {
    provideLinks(bufferLineNumber: number, callback: (links: ILink[] | undefined) => void) {
      try {
        const line: IBufferLine | undefined = term.buffer.active.getLine(bufferLineNumber);
        if (!line) {
          callback(undefined);
          return;
        }
        const text = line.translateToString(true);
        const links = findUrls(text).map(({ start, end, url }) => ({
          range: {
            start: { x: start + 1, y: bufferLineNumber },
            end: { x: end, y: bufferLineNumber },
          },
          text: url,
          activate: (_event: unknown, text: string) => {
            void openExternal(text);
          },
          hover: (_event: unknown, text: string) => {
            term.element?.style.setProperty("cursor", "pointer");
            // 오른쪽 클릭 메뉴가 "링크 열기·링크 주소 복사"를 보일 근거.
            setHoveredLink(term, text);
          },
          leave: () => {
            term.element?.style.removeProperty("cursor");
            setHoveredLink(term, null);
          },
        }));
        callback(links.length > 0 ? links : undefined);
      } catch {
        callback(undefined);
      }
    },
  };
}
