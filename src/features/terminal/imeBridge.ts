/**
 * WebKit(WKWebView) 한글 IME 조정 코어 — 순수 상태기계(node 시험 가능).
 * 계약 전문은 docs/implementation/07-korean-ime.md.
 *
 * macOS WebKit 26의 폴백 경로(두벌식, WKWebView)에서는 조합 이벤트
 * (compositionstart/update/end)가 발생하지 않고 대신
 *  1) 첫 자소: input(insertText "ㅇ") → keydown(keyCode 229, isComposing=false)
 *  2) 조합:    input(insertReplacementText "아"/"안"/…) — 마지막 글자 교체
 *  3) 확정 마커: input(insertReplacementText, 값 변화 없음) — 다음 키가 조합에
 *     붙지 못할 때(새 음절·Space·Enter·기호…) 그 키 직전에 온다. 유휴만으로는
 *     오지 않는다.
 *  4) 실제 키: keydown(실제 keyCode) → input(insertText) — 순서가 반대
 *
 * xterm 6.0은 insertReplacementText를 무시하고(조합 유실), `_keyDownSeen`
 * 게이트가 앞선 keydown을 본 상태면 첫 자소를 버리며(xtermjs#6144),
 * 229 keydown의 diff 타이머는 textarea 값 차이를 재전송한다(중복). 또한
 * 수정키 없는 단일 문자 keydown(keyCode ≥ 48)은 **자모 `key`라도** keydown에서
 * 즉시 전송한다.
 *
 * 조정 전략 — 이 코어가 textarea 입력 이벤트를 처음부터 소유한다:
 *  - insertText(IME 소유 = 자기 keydown이 대기 중이 아닌 것):
 *      · 한글 범위(자모·음절)면 **보류** — 조합이 이어질 텍스트다.
 *      · 그 외(ASCII·기호·이모지 등 조합 없는 삽입)는 즉시 diff로 전송
 *        — 한글 IME의 영문 모드 등이 키마다 끊겨 보이지 않게 한다.
 *  - insertReplacementText(조합): 보류. 확정 신호 시 한 번에 전송. 단, 조합이
 *    이미 보낸 글자를 되돌리면(백스톱 확정 뒤 Backspace 분해) DEL만 즉시 전송.
 *  - 확정 신호(모두 이벤트 기반이라 타이핑 속도와 무관):
 *      · 새 음절의 insertText(앞 음절 완성) · 확정 마커 · 실제 키 keydown
 *        (xterm이 키를 처리하기 전) · 백스톱 타이머.
 *  - 229 keydown은 항상 차단한다 — insert 이벤트를 이 코어가 전부 소유하므로
 *    xterm의 `_handleAnyTextareaChanges` diff 타이머는 중복 전송 위험만 있다.
 *  - **IME 우회 자모 keydown**(229가 아닌 keyCode에 `key`가 호환 자모 — 앱
 *    시작·전환 직후 WebKit이 IME를 거치지 않을 때)은 가로채 두벌식 오토마타로
 *    음절을 조립하고 꼬리 편집으로 즉시 내보낸다(hangulAutomaton.ts).
 *  - **입력 소스 전환 홀드 창**: attach·CapsLock(양방향)·`Unidentified`(keyCode
 *    0) 뒤에는 라틴 인쇄 keydown을 보류한다. 판정은 시간이 아니라 그 키의
 *    keyup `key`로 한다 — 라틴이면 영문 확정(즉시 방출·창 닫힘), 자모면
 *    레이아웃이 한글로 바뀐 것이므로 IME 삽입을 기다린다. IME가 삽입하면
 *    같은 키의 보류는 버리고(IME의 결과다) 앞의 보류는 자판표로 자모 복원해
 *    오토마타에 넣는다. 창이 만료되면 자모 keyup이 있었을 때만 자모로 복원한다.
 *  - compositionstart만 와도 물러나지 않는다 — WebKit은 영→한 전환 직후
 *    이를 단독 발화할 수 있다. 단, attach 계층이 compositionupdate까지
 *    관측하면 해당 compositionupdate~end 구간은 이 코어를 비활성화하고
 *    xterm에 맡긴다(WebKit은 다음 입력에서 다시 폴백으로 돌아갈 수 있다).
 *
 * 표시: 보류 중 텍스트(pendingText)를 attach 계층이 xterm의 인라인
 * preedit(composition view)으로 렌더링한다 — 셸은 확정 시에만 갱신.
 * 오토마타 출력과 홀드 해제 라틴은 textarea 밖의 텍스트라 거울(shellValue/
 * textValue)에 넣지 않는다 — 양쪽이 모르는 텍스트는 꼬리 diff가 건드리지 않는다.
 */

import { HangulAutomaton, isCompatJamo, latinToJamo } from "./hangulAutomaton";

/** PTY 입력의 backspace(DEL). */
export const DEL = "\x7f";

/** 조정기가 내는 동작 — attach 계층이 DOM/내부 API에 적용한다. */
export interface ImeAction {
  /** PTY로 흘려보낼 데이터(조립된 최종열). */
  emit?: string;
  /** 이벤트 전파 차단 — xterm(대상 노드) 리스너가 못 보게 한다. */
  block?: boolean;
  /** 기본 동작 취소 — 전환 홀드 중 인쇄키의 브라우저 삽입·keypress를 막는다. */
  preventDefault?: boolean;
  /** xterm 내부 `_keyDownSeen` 리셋(게이트 유실 방지 — 잔여 방어). */
  resetKeyDownSeen?: boolean;
  /** 백스톱 확정 타이머 (재)가동 — attach 계층이 settle을 예약한다. */
  scheduleSettle?: boolean;
  /** 홀드 창 타이머를 이 지연으로 (재)가동 — attach 계층이 releaseHeldLatin을 예약한다. */
  holdTimerMs?: number;
}

/** 홀드 창 기본 수명 — 앱 시작 직후 IME 활성화(~1초)를 덮는다. keyup 판정이 보통 먼저 끝낸다. */
export const HOLD_WINDOW_MS = 1500;
/** 보류 키의 keyup이 자모였을 때 IME 삽입을 기다리는 시간(실기: keyup 뒤 ~130ms). */
export const HOLD_IME_WAIT_MS = 350;

/**
 * 조정기를 붙일 엔진. 위 계약은 전부 WebKit(WKWebView/Safari) 폴백 IME의
 * 이벤트 순서를 전제한다 — Chromium 계열(WebView2/Chrome/Edge)은 표준
 * compositionstart~end 순서로 xterm 네이티브 조합기가 맞게 처리하므로
 * 여기서 textarea를 소유하면 확정 키(Space·숫자·기호)가 중복·뒤섞인다.
 * 판정: vendor "Apple Computer, Inc." + UA에 Chrome/Chromium/Edg 없음 —
 * 단 Linux는 제외한다. WebKitGTK(Tauri on Linux)는 vendor·UA가 Safari와
 * 같지만 IME는 IBus/fcitx가 표준 compositionstart~end 순서로 전달하므로
 * (macOS 폴백 IME의 역순이 아니다) 이 조정기가 textarea를 소유하면
 * Chromium에서와 같은 확정 키 중복이 난다. WebKitGTK 트레이스를 확보하기
 * 전까지는 xterm 네이티브 조합기에 맡긴다.
 */
export type ImeEngine = "webkit" | "other";

export function detectImeEngine(
  nav: { vendor?: string; userAgent?: string } | undefined = typeof navigator === "undefined"
    ? undefined
    : (navigator as { vendor?: string; userAgent?: string }),
): ImeEngine {
  if (!nav) return "other";
  const vendor = nav.vendor ?? "";
  const ua = nav.userAgent ?? "";
  if (/\bLinux\b/.test(ua) && !/Android/.test(ua)) return "other";
  return vendor === "Apple Computer, Inc." && !/Chrome|Chromium|Edg\//.test(ua) ? "webkit" : "other";
}

/** 수정키 — keydown이 대기 중이어도 IME 삽입의 소유 주장으로 세지 않는다. */
const MODIFIER_KEYS = new Set([16, 17, 18, 20, 91, 92, 93]);

/** 한글 범위(조합용 자모·호환 자모·A960 확장·음절·D7B0 확장). */
const HANGUL_RE = /[ᄀ-ᇿ㄰-㆏ꥠ-꥿가-힣ힰ-퟿]/;

/** 조합이 이어질 수 있는 삽입인가 — 보류 대상. */
function isHoldableText(data: string | undefined): boolean {
  return typeof data === "string" && HANGUL_RE.test(data);
}

/**
 * WebKit은 IME 커밋 계열 삽입에서 스페이스를 줄바꿈 없는 공백(U+00A0)으로
 * textarea에 박는다. 이걸 그대로 diff로 내보내면 셸은 스페이스를 두 번
 * 받고(하나는 xterm keydown, 하나는 NBSP), 우리 상태(shellValue)는 셸 라인과
 * 어긋나 이후 확정 diff가 한 칸씩 밀린다(자모 유출·글자 뒤섞임의 원인).
 * 숨은 textarea의 NBSP는 셸에서도 똑같이 일반 공백으로 취급한다.
 */
function normalizeImeText(value: string): string {
  return value.replace(/ /g, " ");
}

/**
 * 전송 우선권 판정용 키 코드 — 스페이스와 ASCII 인쇄 문자만 해당하고
 * 나머지(한글·기호의 실제 keyCode 불일치)는 매칭 불가(null)로 둔다.
 * 매칭이 빗나가도 아무 일도 일어나지 않는 안전한 방향이다.
 */
function keyCodeForText(data: string): number | null {
  if (data === "\r" || data === "\n") return 13;
  if (data === "\t") return 9;
  if (data.length !== 1) return null;
  const code = data.toUpperCase().charCodeAt(0);
  return code >= 32 && code <= 126 ? code : null;
}

/**
 * 꼬리 편집 diff: 공통 접두사 이후를 "지우고 다시 쓰기"로 표현한다.
 * 조합 교체는 항상 끝의 마지막 글자에서 일어나므로 이 표현으로 충분하다.
 * 코드포인트 단위(서로게이트 쌍 안전).
 */
export function tailEdit(prev: string, next: string): { deletes: number; insert: string } {
  const prevChars = Array.from(prev);
  const nextChars = Array.from(next);
  let common = 0;
  while (common < prevChars.length && common < nextChars.length && prevChars[common] === nextChars[common]) {
    common++;
  }
  return { deletes: prevChars.length - common, insert: nextChars.slice(common).join("") };
}

/** 홀드 창에서 보류 중인 라틴 인쇄 keydown 하나. */
interface HeldKey {
  keyCode: number;
  char: string;
  /** 이 키의 keyup이 자모 `key`로 왔다 — 레이아웃이 한글로 바뀐 신호. */
  jamoKeyup: boolean;
  /** keyup이 알려준 자모(Shift 자모 포함) — 복원 시 자판표보다 우선한다. */
  jamo: string | null;
}

/** keydown이 상태에 아무 영향도 주지 않는 키인가(입력 소스 전환 잡음·수정키). */
function isTransparentKey(keyCode: number, key: string | undefined): boolean {
  return keyCode === 0 || key === "Unidentified" || MODIFIER_KEYS.has(keyCode);
}

function isLatinLetter(ch: string | undefined): boolean {
  return typeof ch === "string" && ch.length === 1 && /[A-Za-z]/.test(ch);
}

/** keyup `key`가 호환 자모인가 — 타입 가드가 아닌 판정(else 분기에서 string을 잃지 않게). */
function isJamoKeyName(key: string | undefined): boolean {
  return typeof key === "string" && key.length === 1 && key >= "ㄱ" && key <= "ㅣ";
}

function mergeEmit(prefix: string, action: ImeAction): ImeAction {
  if (!prefix) return action;
  return { ...action, emit: prefix + (action.emit ?? "") };
}

export class WebKitImeCore {
  /** 셸 라인이 가진 상태(우리가 전달한 누적)와 직전 beforeinput 스냅숏. */
  private shellValue = "";
  private textValue = "";
  /** 조합 보류 중 — 확정 신호까지 전송하지 않는다. */
  private pending = false;
  /** 아직 keyup을 못 만난, 인쇄 가능 키의 keydown(이벤트 소유자). */
  private pendingPrintableKey: number | null = null;
  /** 직전 beforeinput(insertText)의 IME 소유 여부. */
  private lastInsertImeOwned = false;
  /** Space keydown을 브리지가 먼저 차단한 뒤 도착하는 insertText인가. */
  private lastInsertCapturedSpace = false;
  /** 차단한 Space의 기본 textarea 삽입을 기다리는 중(keyup 뒤에도 유지). */
  private capturedSpacePending = false;
  /** xterm keydown이 전송을 맡은 마지막 인쇄키 — 늦은 input의 이중 전송 스킵. */
  private lastPassedPrintableKey: number | null = null;
  /** 우리가 전송한 직후의 키 — 뒤따르는 같은 keydown 차단(이중 전송 봉쇄). */
  private selfEchoedKey: number | null = null;
  /** 자체 전송 키의 keypress 차단(xterm은 대문자를 keypress에서 보낸다). */
  private suppressKeypressKey: number | null = null;
  /** 입력 소스 전환 불확실 구간 — 라틴 인쇄 keydown을 보류한다. */
  private holdWindow = false;
  /** 홀드 창에서 보류 중인 키들(입력 순). */
  private held: HeldKey[] = [];
  /** IME 우회 자모 keydown을 조립하는 두벌식 오토마타. */
  private readonly automaton = new HangulAutomaton();
  /**
   * 오토마타의 현재 음절이 홀드 라틴의 자모 복원으로 만들어졌다 — 늦은 IME가
   * 같은 자모를 삽입하면(IME도 그 키를 처리한 것) 마지막 자모를 되돌린다.
   */
  private automatonTentative = false;

  // 확정은 이벤트 신호(마커·다음 자소·실제 키)가 담당하며 타이핑 속도와
  // 무관하다. 이 타이머는 조합이 묶인 채 멈추는 병적 상태를 푸는 안전망일
  // 뿐 — preedit가 화면을 책임지므로 중간 상태를 시간으로 플러시하지
  // 않는다(느린 타이핑에서 줄로 새어 나가 이중 표시된다).
  constructor(private readonly settleMs: number = 2000) {}

  /** 백스톱 지연 — attach 계층의 타이머 예약에 쓴다. */
  get settleDelayMs(): number {
    return this.settleMs;
  }

  /**
   * 아직 셸로 보내지 않은 꼬리 — 인라인 preedit(조합 중 표시)용.
   * shellValue와의 공통 접두사 이후 부분으로, 보류 중이 아니면 비어 있다.
   */
  get pendingText(): string {
    const settled = Array.from(this.shellValue);
    const current = Array.from(this.textValue);
    let common = 0;
    while (common < settled.length && common < current.length && settled[common] === current[common]) {
      common++;
    }
    return current.slice(common).join("");
  }

  /** 오토마타(IME 우회 경로)가 조합 중인 음절 — 이미 셸에 나가 있다. 진단·시험용. */
  get automatonText(): string {
    return this.automaton.current;
  }

  /** 홀드 창이 열려 있는가(시험·진단용). */
  get holding(): boolean {
    return this.holdWindow;
  }

  /**
   * attach 계층이 확정된 hidden textarea를 비운 뒤 로컬 diff 기준점만
   * 재설정한다. 늦게 오는 keydown/keypress의 이중 전송 차단 상태는
   * 보존해야 하므로 resync(전체 이벤트 상태 초기화)와 구분한다.
   */
  textAreaCleared(): void {
    this.shellValue = "";
    this.textValue = "";
  }

  /**
   * 외부에서 바뀐 textarea 값을 새 거울 기준점으로 삼는다 — 셸 라인과의 대응은
   * 끊긴 것이므로 보류 조합도 버린다(이미 textarea에서 사라져 diff로 낼 수 없다).
   * 늦은 keydown/keypress의 이중 전송 차단 상태는 보존한다(resync와의 차이).
   */
  private rebase(value: string): void {
    this.shellValue = value;
    this.textValue = value;
    this.pending = false;
  }

  /**
   * WebKit은 영→한 전환 직후 첫 조합에서 compositionstart를 단독으로
   * 발화한다(이후 조합은 여전히 insertText/insertReplacementText로 온다).
   * 여기서 물러나면(무장 해제) 이후 모든 입력이 WebKit에서 조합을 유실하는
   * xterm 네이티브 경로로 넘어가 자모 유출·글자 뒤섞임이 된다 — 따라서
   * 해제하지 않고 보류분만 확정한다. IME가 개입했으므로 오토마타 음절도 확정.
   */
  compositionstart(): ImeAction {
    this.automaton.commit();
    this.automatonTentative = false;
    return this.settle();
  }

  /**
   * 입력 소스 전환 불확실 구간을 연다 — attach 직후, CapsLock(방향 무관),
   * `Unidentified`(keyCode 0) keydown 뒤. 열려 있는 동안 라틴 인쇄 keydown은
   * 보류되고 그 키의 keyup `key`(라틴/자모)와 IME 삽입 여부로 판정된다.
   * 호출자는 `holdTimerMs` 뒤에 releaseHeldLatin()을 예약해야 한다.
   */
  armInputSourceHold(): ImeAction {
    const flush = this.settle();
    this.holdWindow = true;
    return { ...flush, holdTimerMs: HOLD_WINDOW_MS };
  }

  /** @deprecated armInputSourceHold — 이름만 남긴 호환 별칭. */
  armKoreanActivationHold(): ImeAction {
    return this.armInputSourceHold();
  }

  /**
   * 앱이 입력 소스를 직접 전환한다 — Shift+Space 한/영 강제 전환(hangulToggle.ts).
   * 전환 키는 PTY 입력이 아니라 비인쇄 키처럼 다룬다: 보류 키(자모 keyup이 있었으면
   * 자모 복원)·오토마타 음절·IME 조합을 먼저 확정해 입력 순서를 지키고, CapsLock과
   * 같은 이유로 전환 직후 첫 키들을 홀드 창으로 가른다(WebKit이 전환 직후 IME를
   * 거치지 않는 경주 — 07-korean-ime §3-1). 호출자가 그 keydown을 preventDefault하므로
   * 짝 keypress·textarea 삽입은 오지 않는다 — keydown 한 번처럼 에코·keypress 억제
   * 표식을 소진한다.
   */
  inputSourceToggled(): ImeAction {
    this.selfEchoedKey = null;
    this.suppressKeypressKey = null;
    const release = this.releaseHeldLatin();
    this.automaton.commit();
    this.automatonTentative = false;
    return mergeEmit(release.emit ?? "", this.armInputSourceHold());
  }

  /**
   * 홀드 창 만료(타이머) — 보류 키 중 하나라도 keyup이 자모였으면 사용자는
   * 한글을 치는 중이고 IME는 아직 붙지 않은 것이므로 자판표로 자모 복원해
   * 오토마타에 넣는다. 아니면 영문 타이핑이므로 라틴을 그대로 내보낸다.
   * 어느 쪽이든 창은 닫힌다. 내보낸 텍스트는 textarea에 없다(preventDefault로
   * 막았다) — shellValue에도 넣지 않는다(거울 불변식).
   */
  releaseHeldLatin(): ImeAction {
    if (!this.holdWindow) return {};
    this.holdWindow = false;
    const keys = this.held;
    this.held = [];
    const emit = keys.some((h) => h.jamoKeyup) ? this.absorbHeldAsJamo(keys) : keys.map((h) => h.char).join("");
    return emit ? { emit } : {};
  }

  /** 보류 키들을 두벌식 자판표로 자모 복원해 오토마타에 넣는다(매핑 불가 문자는 그대로). */
  private absorbHeldAsJamo(keys: HeldKey[]): string {
    let emit = "";
    let absorbed = false;
    for (const h of keys) {
      const jamo = h.jamo ?? latinToJamo(h.char);
      if (jamo !== null) {
        emit += this.automaton.feed(jamo);
        absorbed = true;
      } else {
        this.automaton.commit();
        emit += h.char;
      }
    }
    if (absorbed) this.automatonTentative = true;
    return emit;
  }

  /**
   * IME가 삽입을 시작했다(홀드 창 판정): 삽입된 자모와 같은 키의 보류는 IME의
   * 결과이므로 버리고, 그 앞의 보류(IME가 삼킨 키)는 자모 복원한다. 뒤의 보류는
   * IME가 이어서 처리할 키다. 일치하는 키가 없으면 전부 자모 복원한다.
   */
  private resolveHeldOnImeInsert(data: string | undefined): string {
    if (!this.holdWindow) return "";
    this.holdWindow = false;
    const keys = this.held;
    this.held = [];
    if (keys.length === 0) return "";
    let match = -1;
    if (isCompatJamo(data)) {
      for (let i = keys.length - 1; i >= 0; i--) {
        if ((keys[i].jamo ?? latinToJamo(keys[i].char)) === data) {
          match = i;
          break;
        }
      }
    }
    if (match < 0) return this.absorbHeldAsJamo(keys);
    return this.absorbHeldAsJamo(keys.slice(0, match));
  }

  /** 변경 전 값(value — WebKit에서는 아직 이전 상태)으로 스냅숏을 갱신. */
  beforeinput(inputType: string, value: string): ImeAction {
    const normalized = normalizeImeText(value);
    if (normalized !== this.textValue) {
      // textarea가 우리 모르게 바뀌었다 — xterm은 blur(클릭·다른 패널·창 전환)
      // 에서 textarea.value를 비운다(_handleTextAreaBlur). 그 값을 새 기준점으로
      // 삼는다: 사라진 텍스트는 셸에서 지운 것도, 다시 보낼 것도 아니다. 기준을
      // 안 옮기면 다음 스페이스·확정 diff가 옛 shellValue 길이만큼 DEL을 쏟아
      // xterm이 보낸 라틴까지 지운다(ccd + 스페이스 → DEL×5, "cc만 남음").
      this.rebase(normalized);
    }
    this.textValue = normalized;
    this.lastInsertCapturedSpace = inputType === "insertText" && this.capturedSpacePending;
    if (this.lastInsertCapturedSpace) this.capturedSpacePending = false;
    this.lastInsertImeOwned = inputType === "insertText" && (
      this.pendingPrintableKey === null || this.lastInsertCapturedSpace
    );
    const imeInput = inputType === "insertText" || inputType === "insertReplacementText";
    // 자기 keydown이 대기 중인 insertText는 실제 키 경로다 — 게이트가 이중
    // 입력을 맞게 두고, 그렇지 않은(IME가 먼저 넣은) 입력만 리셋해 유실을
    // 막는다.
    if (imeInput && this.pendingPrintableKey === null) return { resetKeyDownSeen: true };
    return {};
  }

  /** 변경 후 값(value)으로 소유권을 결정한다. `data`는 이벤트의 그것. */
  input(inputType: string, value: string, data?: string): ImeAction {
    // xterm 전송 우선권 판정에만 쓰고 바로 소비한다 — 오래 된 키에 묶여
    // 스킵 판정이 남아 다음 글자를 삼키는 일이 없게 한다.
    const passedKey = this.lastPassedPrintableKey;
    this.lastPassedPrintableKey = null;
    const capturedSpace = this.lastInsertCapturedSpace;
    this.lastInsertCapturedSpace = false;
    const nextValue = normalizeImeText(value);
    const imeInsert = inputType === "insertReplacementText" || isHoldableText(data);
    // IME가 조합을 내기 시작했다는 신호 — 홀드 창을 판정하고, 오토마타 음절은
    // 확정한다(IME가 이어서 조합하므로 더 붙일 수 없다). 홀드 자모 복원 직후
    // 늦은 IME가 같은 자모를 넣으면 오토마타의 그 자모를 되돌린다(중복 방지).
    let prefix = imeInsert ? this.resolveHeldOnImeInsert(data) : "";
    if (this.automatonTentative && isCompatJamo(data) && data === this.automaton.lastJamo) {
      prefix += this.automaton.backspace() ?? "";
    }
    this.automatonTentative = false;
    this.automaton.commit();
    return mergeEmit(prefix, this.processInput(inputType, nextValue, data, passedKey, capturedSpace));
  }

  private processInput(
    inputType: string,
    nextValue: string,
    data: string | undefined,
    passedKey: number | null,
    capturedSpace: boolean,
  ): ImeAction {
    if (inputType === "insertReplacementText") {
      if (nextValue === this.textValue) {
        // 값 변화가 없는 확정 마커 — 대기 중 조합을 지금 내보낸다.
        return { ...this.settle(), block: true };
      }
      this.textValue = nextValue;
      this.pending = true;
      const retract = this.retractCommitted();
      return retract
        ? { emit: retract, block: true, scheduleSettle: true }
        : { block: true, scheduleSettle: true };
    }
    if (inputType !== "insertText") {
      // 값이 짧아지는 변경(deleteContentBackward 등)은 사용자의 백스페이스
      // 계열이다 — xterm이 이미 0x7f를 셸로 보냈으므로 shellValue도 같이
      // 줄인다. 동기화하지 않으면 이후 확정 diff가 취소 DEL을 내보내
      // 방금 입력한 글자를 지워버린다(영어→전체 삭제→안녕에서 안 유실).
      // 조합 보류 중의 삭제는 조합 수정이므로 건드리지 않는다.
      if (
        nextValue.length < this.textValue.length &&
        !this.pending &&
        this.shellValue === this.textValue
      ) {
        this.shellValue = nextValue;
      }
      this.textValue = nextValue;
      return {};
    }
    // beforeinput 누락 엔진 방어: input 시점에도 대기 인쇄키로 재확인.
    // 삽입 데이터가 한글 범위면 이는 IME 조합이 확실하다 — 실제 라틴 키는
    // 한글 insertText를 만들지 않는다. 라틴 키의 keyup이 롤오버(빠른 타이핑
    // 겹침)로 늦게 도착해 pendingPrintableKey가 남아 있어도, 조합을 실제
    // 키로 오인해 shellValue를 건너뛰지 않는다 — 그 오인은 이어지는 확정
    // diff의 DEL이 앞 라틴 글자를 먹어버린다(hello안녕 → hell안녕).
    const imeOwned = isHoldableText(data) || (this.lastInsertImeOwned && (
      this.pendingPrintableKey === null || capturedSpace
    ));
    this.lastInsertImeOwned = false;
    if (!imeOwned) {
      // 실제 키가 소유 — xterm keydown이 이미 보냈다. 상태만 동기화.
      this.textValue = nextValue;
      this.shellValue = nextValue;
      return { block: true };
    }
    // 새 음절의 시작은 앞 음절의 완성: 보류분을 먼저 확정한다.
    const flush = this.settle();
    this.textValue = nextValue;
    if (!isHoldableText(data)) {
      const code = keyCodeForText(normalizeImeText(typeof data === "string" ? data : ""));
      if (code !== null && code === passedKey) {
        // 이 키는 xterm keydown이 이미 전송했다(input이 keyup보다 늦게
        // 도착하는 WebKit 순서). 우리가 다시 보내면 스페이스가 두 번
        // 찍힌다 — 상태만 동기화한다.
        this.shellValue = nextValue;
        return flush.emit ? { emit: flush.emit, block: true } : { block: true };
      }
      // 조합이 이어지지 않는 삽입(영문 모드·기호·스페이스) — 즉시 전송.
      // 뒤따르는 같은 키의 keydown은 우리가 이미 보냈으니 차단해야 한다.
      const { deletes, insert } = tailEdit(this.shellValue, nextValue);
      this.shellValue = nextValue;
      const diff = deletes > 0 || insert.length > 0 ? DEL.repeat(deletes) + insert : "";
      // input이 먼저 온 키만 뒤따르는 keydown을 막아야 한다. Space keydown을
      // 우리가 먼저 차단한 경우에는 이미 그 이벤트가 끝났으므로 echo 표식을
      // 남기면 다음 Space를 잘못 삼킬 수 있다.
      if (diff && !capturedSpace) this.selfEchoedKey = code;
      const emit = (flush.emit ?? "") + diff;
      return emit ? { emit, block: true } : { block: true };
    }
    this.pending = true;
    const emit = (flush.emit ?? "") + this.retractCommitted();
    return emit
      ? { emit, block: true, scheduleSettle: true }
      : { block: true, scheduleSettle: true };
  }

  /**
   * 조합이 이미 셸로 보낸 꼬리를 되돌리는 경우 — 유휴 백스톱이 확정한 음절을
   * WebKit IME는 아직 조합 중이라 Backspace가 분해한다(요→ㅇ, 안→아). 되돌린
   * 글자 수만큼 DEL을 즉시 내보내고 셸 기준점을 공통 접두사로 줄인다. 삽입분은
   * preedit(pendingText)로 남아 다음 확정 신호에 나간다. 이를 백스톱까지 미루면
   * 화면에 확정된 요와 preedit ㅇ이 겹쳐 보이다가(하→하ㅎ) 2초 뒤에야 합쳐진다.
   */
  private retractCommitted(): string {
    const { deletes } = tailEdit(this.shellValue, this.textValue);
    if (deletes === 0) return "";
    const kept = Array.from(this.shellValue);
    this.shellValue = kept.slice(0, kept.length - deletes).join("");
    return DEL.repeat(deletes);
  }

  /**
   * keydown. `key`는 이벤트의 `key` 원문(단일 문자면 인쇄 문자, "Enter"·
   * "Backspace"·"Unidentified" 같은 이름일 수 있다).
   *  - 수정키·keyCode 0·"Unidentified"는 투명하다(상태 불변).
   *  - 229가 아닌 keyCode에 자모 `key` → IME 우회. 오토마타로 조립·즉시 전송.
   *  - 홀드 창 중 라틴 인쇄키 → 보류. Backspace는 보류 키를 되돌린다.
   *  - 그 밖의 실제 키는 조합·오토마타를 확정한 뒤 xterm이 처리한다. 229는 차단.
   */
  keydown(
    keyCode: number,
    isComposing: boolean,
    hasCommandModifier = false,
    key?: string,
  ): ImeAction {
    if (isTransparentKey(keyCode, key)) return {};
    // keypress 억제는 직전 keydown과 짝을 이룰 때만 유효하다 — 새 keydown이
    // 오면 초기화해 오래 남은 억제가 나중 글자를 삼키지 않게 한다.
    this.suppressKeypressKey = null;
    // 우리가 이미 전송한 키의 뒤늦은 keydown(IME 커밋의 input 선행 순서) —
    // xterm이 같은 글자를 또 보내지 못하게 막는다. 대문자는 xterm이
    // keypress에서 보내므로 그 짝인 keypress까지 함께 막는다.
    if (this.selfEchoedKey !== null) {
      const echoed = this.selfEchoedKey;
      this.selfEchoedKey = null;
      this.suppressKeypressKey = echoed;
      if (echoed === keyCode) return { ...this.settle(), block: true };
    }
    const printable = typeof key === "string" && key.length === 1 ? key : undefined;
    // IME 우회 자모 keydown — xterm이 자모를 그대로 보내기 전에 가로채 조립한다.
    if (keyCode !== 229 && !hasCommandModifier && isCompatJamo(printable)) {
      let emit = this.settle().emit ?? "";
      if (this.holdWindow) {
        // 레이아웃은 한글로 바뀌었고 IME는 아직 안 붙었다 — 앞서 보류한
        // 라틴은 한글 의도였다. 창은 유지한다(뒤따르는 라틴 keydown은 늦은
        // IME 삽입의 전조일 수 있다). 타이머는 연장.
        const keys = this.held;
        this.held = [];
        emit += this.absorbHeldAsJamo(keys);
      }
      this.automatonTentative = false;
      emit += this.automaton.feed(printable);
      const action: ImeAction = { block: true, preventDefault: true };
      if (emit) action.emit = emit;
      if (this.holdWindow) action.holdTimerMs = HOLD_WINDOW_MS;
      return action;
    }
    if (keyCode === 8 && !hasCommandModifier && this.holdWindow && this.held.length > 0) {
      // 보류 키는 아직 셸에 없다 — 목록에서 되돌리면 끝.
      this.held.pop();
      return { block: true, preventDefault: true };
    }
    if (keyCode === 8 && !hasCommandModifier && this.automaton.current) {
      const emit = this.automaton.backspace();
      this.automatonTentative = false;
      return emit ? { emit, block: true, preventDefault: true } : { block: true, preventDefault: true };
    }
    // 자모가 아닌 실제 키 — 오토마타 음절은 여기서 확정된다(이미 셸에 있다).
    this.automaton.commit();
    this.automatonTentative = false;
    if (
      this.holdWindow &&
      !hasCommandModifier &&
      keyCode !== 229 &&
      keyCode !== 32 &&
      keyCode !== 9
    ) {
      if (printable !== undefined) {
        this.held.push({ keyCode, char: printable, jamoKeyup: false, jamo: null });
        return { ...this.settle(), block: true, preventDefault: true };
      }
      // 비인쇄 키(Enter·방향키·Backspace…) — 보류를 먼저 해소해 입력 순서를
      // 지키고 키를 정상 처리하게 한다.
      const release = this.releaseHeldLatin();
      return mergeEmit(release.emit ?? "", this.processKeydown(keyCode, isComposing, hasCommandModifier));
    }
    if (keyCode === 229 && this.holdWindow) {
      // IME가 붙었다(229) — 그런데 보류 키가 남아 있다면 IME가 삼킨 키다.
      const release = this.releaseHeldLatin();
      return mergeEmit(release.emit ?? "", this.processKeydown(keyCode, isComposing, hasCommandModifier));
    }
    return this.processKeydown(keyCode, isComposing, hasCommandModifier);
  }

  private processKeydown(
    keyCode: number,
    isComposing: boolean,
    hasCommandModifier: boolean,
  ): ImeAction {
    // 공백은 xterm keydown과 WebKit의 늦은 NBSP input 양쪽에서 관찰될 수
    // 있다. 수정키 없는 Space의 keydown/keypress를 여기서 차단하되
    // preventDefault는 하지 않아 textarea 기본 삽입을 살리고, 그 input을
    // 브리지가 유일하게 PTY로 보낸다. 전송 주체가 하나라 빠른 후속 한글
    // 조합과 이벤트 순서가 교차해도 공백이 유실되거나 중복되지 않는다.
    if (keyCode === 32 && !isComposing && !hasCommandModifier) {
      this.pendingPrintableKey = keyCode;
      this.capturedSpacePending = true;
      this.suppressKeypressKey = keyCode;
      return { ...this.settle(), block: true };
    }
    if (keyCode === 229) {
      // IME가 처리한 키. 조합 중(isComposing)이면 xterm의 CompositionHelper가
      // 맡고, 아니면 차단한다 — insert 이벤트는 이 코어가 전부 소유하므로
      // xterm의 diff 타이머(_handleAnyTextareaChanges)는 중복 전송 위험뿐이다.
      return isComposing ? {} : { block: true };
    }
    this.pendingPrintableKey = keyCode;
    // 실제 키 — xterm이 키를 처리하기 전에 대기 조합을 내보낸다. 이 키의
    // 전송은 xterm이 맡으니, 늦게 오는 input은 우리가 반복하지 않는다.
    // Ctrl/Alt/Meta 조합은 같은 keyCode라도 실제 전송 데이터가 다르므로
    // 뒤이은 일반 insertText의 중복 판정 후보로 남기지 않는다.
    this.lastPassedPrintableKey = hasCommandModifier ? null : keyCode;
    return this.settle();
  }

  /**
   * keypress: 자체 전송한 키의 keypress만 차단한다. xterm은 A–Z 대문자를
   * keydown이 아니라 keypress에서 보내므로(macOS IME HACK), keydown만 막으면
   * 여기서 이중이 된다.
   */
  keypress(keyCode: number): ImeAction {
    // 홀드 창 중의 keypress — keydown을 preventDefault 했으므로 원래 오지
    // 않지만, 왔다는 건 홀드 키의 짝이다. xterm이 보내지 못하게 막는다.
    if (this.holdWindow && this.held.length > 0) return { block: true };
    if (this.suppressKeypressKey !== null) {
      const k = this.suppressKeypressKey;
      this.suppressKeypressKey = null;
      if (k === keyCode) return { block: true };
    }
    return {};
  }

  /**
   * keyup. 홀드 창의 판정 신호: 보류 키의 keyup `key`가 라틴이면 영문 타이핑이
   * 확정돼 보류 전부를 즉시 내보내고 창을 닫는다. 자모면 레이아웃이 한글로
   * 바뀐 것이므로 IME 삽입을 짧게 기다린다(호출자는 holdTimerMs로 타이머 재설정).
   */
  keyup(keyCode: number, key?: string): ImeAction {
    if (this.pendingPrintableKey === keyCode) this.pendingPrintableKey = null;
    if (!this.holdWindow || this.held.length === 0) return {};
    const entry = [...this.held].reverse().find((h) => h.keyCode === keyCode && !h.jamoKeyup);
    if (!entry) return {};
    if (isJamoKeyName(key)) {
      entry.jamoKeyup = true;
      entry.jamo = key ?? null;
      return { holdTimerMs: HOLD_IME_WAIT_MS };
    }
    if (isLatinLetter(key) || (typeof key === "string" && key.length === 1 && key === entry.char)) {
      this.holdWindow = false;
      const emit = this.held.map((h) => h.char).join("");
      this.held = [];
      return emit ? { emit } : {};
    }
    return {};
  }

  /** 백스톱/확정 신호: 대기 중 조합을 꼬리 diff로 한 번에 내보낸다. */
  settle(): ImeAction {
    if (!this.pending) {
      this.pending = false;
      return {};
    }
    this.pending = false;
    const { deletes, insert } = tailEdit(this.shellValue, this.textValue);
    this.shellValue = this.textValue;
    if (deletes === 0 && insert.length === 0) return {};
    return { emit: DEL.repeat(deletes) + insert };
  }

  /**
   * keydown 이후 xterm이 textarea를 비우면(CR/ETX/blur) 상태를 재동기화.
   * 홀드 창은 건드리지 않는다 — attach 직후 창이 첫 Enter·blur로 닫히면 시작
   * 경주를 못 덮는다. 보류 키는 이미 그 키(Enter 등)에서 해소됐다.
   */
  resync(value: string): void {
    const normalized = normalizeImeText(value);
    this.shellValue = normalized;
    this.textValue = normalized;
    this.pending = false;
    this.pendingPrintableKey = null;
    this.lastInsertImeOwned = false;
    this.lastInsertCapturedSpace = false;
    this.capturedSpacePending = false;
    this.lastPassedPrintableKey = null;
    this.selfEchoedKey = null;
    this.suppressKeypressKey = null;
    this.automaton.commit();
    this.automatonTentative = false;
  }
}
