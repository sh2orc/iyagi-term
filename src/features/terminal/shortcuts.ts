/**
 * 단축키 계약 (04-ui.md §3, design §10).
 *
 * - macOS Cmd+D / Cmd+Shift+D; Windows/Linux Ctrl+Shift+D / Ctrl+Shift+E.
 * - pane 닫기 Cmd+W / Ctrl+Shift+W, palette Ctrl/Cmd+Shift+P,
 *   copy Cmd+C(mac, selection 없으면 no-op) / Ctrl+Shift+C,
 *   paste Cmd+V / Ctrl+Shift+V.
 * - 동기 입력 토글 Cmd+Shift+B / Ctrl+Shift+B(04 §3). Alt 조합은 터미널의
 *   Meta로 남겨 두므로 iTerm2의 Cmd+Alt+I 대신 기존 Shift 계열을 따른다.
 * - 배치 편집 화면 열기/닫기 Cmd+Shift+G / Ctrl+Shift+G.
 * - 확대/축소 Cmd/Ctrl+= · Cmd/Ctrl+- · 되돌리기 Cmd/Ctrl+0(04 §3 zoom).
 *   pane 생성 계열과 달리 key repeat을 허용한다(누르고 있으면 연속 조정).
 * - scrollback 검색 Cmd+F / Ctrl+Shift+F(W1-9).
 * - 탭 순환 Cmd+Shift+] / Cmd+Shift+[ (macOS), Ctrl+PageDown / Ctrl+PageUp(그 외)
 *   — 브라우저·iTerm2·Windows Terminal의 관행을 그대로 따른다. 트랙패드 두
 *   손가락 가로 스와이프(tabSwipe.ts)와 같은 동작이다.
 * - 새 AI 작업 Cmd+Shift+M / Ctrl+Shift+M. macOS의 Cmd+M(최소화)은 Shift가 없어
 *   겹치지 않고, 그 외 플랫폼은 다른 앱 조합과 같은 Ctrl+Shift 계열이라 맨
 *   Ctrl+M(CR)은 그대로 터미널에 간다.
 * - 일시정지 모두 재개 Cmd+Shift+R / Ctrl+Shift+R(08 §5). 자원 가드가 일시정지한
 *   작업 전체를 한 번에 재개한다 — 맨 Cmd+R·맨 Ctrl+R은 터미널에 남겨 둔다.
 * - Windows/Linux Ctrl+D는 셸 EOF(0x04)로 전달(=앱 액션 아님),
 *   Ctrl+C는 선택 유무와 무관하게 터미널 interrupt 입력.
 * - 앱 단축키에 일치하는 경우에만 preventDefault한다(반환값 non-null).
 * - IME composition / modal / key repeat 중 pane 생성·포커스 변경 계열은 발동하지 않는다.
 */

export type Platform = "darwin" | "windows" | "linux";

export type ShortcutAction =
  | "split-row" // 좌우 배치 분할(세로 분할선)
  | "split-column" // 상하 배치 분할(가로 분할선)
  | "close-pane"
  | "palette"
  | "queue-toggle" // 오른쪽 관리 실행 패널 열기/닫기
  | "broadcast-toggle" // 같은 탭 모든 pane에 동시 입력(기본 off)
  | "layout-editor" // 배치 편집 화면 열기/닫기(Cmd+Shift+G / Ctrl+Shift+G)
  | "copy"
  | "paste"
  | "search" // 현재 scrollback 검색(W1-9): Cmd+F / Ctrl+Shift+F
  | "zoom-in" // 포커스 pane 글꼴 확대(Ctrl/Cmd+=, numpad +)
  | "zoom-out" // 포커스 pane 글꼴 축소(Ctrl/Cmd+-, numpad -)
  | "zoom-reset" // 기본 글꼴로 되돌리기(Ctrl/Cmd+0)
  | "next-tab" // 다음 탭(Cmd+Shift+] / Ctrl+PageDown)
  | "prev-tab" // 이전 탭(Cmd+Shift+[ / Ctrl+PageUp)
  | "new-mission" // 새 AI 작업 대화상자(Cmd+Shift+M / Ctrl+Shift+M)
  | "resume-all"; // 일시정지(자원 가드) 작업 전체 재개(Cmd+Shift+R / Ctrl+Shift+R)

export interface KeyEventLike {
  key: string;
  /** Physical key code (`KeyD`, `KeyE`, ...) — IME 레이아웃과 무결. */
  code: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  repeat: boolean;
}

export interface ShortcutContext {
  platform: Platform;
  /** compositionstart~compositionend 사이 true. */
  imeComposing: boolean;
  /** palette/붙여넣기 미리보기 등 modal이 열려 있으면 앱 단축키 차단. */
  modalOpen: boolean;
  /** macOS Cmd+C no-selection no-op 판정용. */
  hasSelection: boolean;
  /** 사용자 재정의 바인딩(W3-2). 가드보다 뒤에서, 기본표보다 앞에서 적용. */
  overrides?: ShortcutOverrides;
}

/**
 * 사용자 재정의 바인딩 하나(W3-2). 물리 `code` + 수정키 조합.
 * Alt는 터미널의 Meta로 예약돼 재정의 대상이 아니다(조건 ②).
 */
export interface KeyBinding {
  code: string;
  ctrl: boolean;
  meta: boolean;
  shift: boolean;
}

/** 액션별 재정의. "pass"는 그 조합을 터미널로 되돌려 보낸다. */
export type ShortcutOverrides = Partial<Record<ShortcutAction, KeyBinding | "pass">>;

/** 재정의할 수 있는 액션 목록(줌 리셋 등 포함, 전체). */
export const OVERRIDABLE_ACTIONS: readonly ShortcutAction[] = [
  "split-row",
  "split-column",
  "close-pane",
  "palette",
  "queue-toggle",
  "broadcast-toggle",
  "layout-editor",
  "copy",
  "paste",
  "search",
  "zoom-in",
  "zoom-out",
  "zoom-reset",
  "next-tab",
  "prev-tab",
  "new-mission",
  "resume-all",
];

/** 바인딩이 터미널 통과키(예약)인가 — 재정의 대상으로 금지(조건 ②). */
export function isReservedForTerminal(binding: KeyBinding): boolean {
  // 맨 Ctrl 조합(Ctrl+C interrupt, Ctrl+D EOF, Ctrl+F forward-char…)은
  // 셸 입력이다. 관리 실행 패널에 할당한 Ctrl+B만 예외다.
  if (binding.ctrl && !binding.meta && !binding.shift && binding.code !== "KeyB") return true;
  // 수정키 없는 맨 키도 터미널 입력이다.
  if (!binding.ctrl && !binding.meta) return true;
  return false;
}

function sameBinding(a: KeyBinding, b: KeyBinding): boolean {
  return a.code === b.code && a.ctrl === b.ctrl && a.meta === b.meta && a.shift === b.shift;
}

/**
 * 재정의 집합 검증(조건 ③·④). 문제는 [액션, 사유] 쌍으로 반환한다.
 *  - 예약 조합(터미널 통과키)은 거절
 *  - 서로 같은 조합으로 충돌하는 액션은 거절
 *  - 다른 액션의 **기본** 조합과 충돌하면 거절(기본표는 그대로 쓰게)
 */
export function validateShortcutOverrides(
  overrides: ShortcutOverrides,
  platform: Platform,
): Array<[ShortcutAction, string]> {
  const errors: Array<[ShortcutAction, string]> = [];
  const entries = Object.entries(overrides).filter(([, value]) => value !== undefined) as Array<
    [ShortcutAction, KeyBinding | "pass"]
  >;
  const taken = new Map<string, ShortcutAction>();
  for (const [action, value] of entries) {
    if (value === "pass") continue;
    if (isReservedForTerminal(value)) {
      errors.push([action, "reserved"]);
      continue;
    }
    // Windows의 meta는 Win 키, Linux(GNOME/KDE)에서는 Super 키다 — 둘 다
    // OS가 먼저 가져가므로 앱 단축키로 쓸 수 없다(바인딩해도 절대 오지 않는다).
    if ((platform === "windows" || platform === "linux") && value.meta) {
      errors.push([action, "reserved"]);
      continue;
    }
    const key = bindingKey(value);
    const clash = taken.get(key);
    if (clash) {
      errors.push([action, `conflicts:${clash}`]);
      continue;
    }
    const defaultClash = defaultBindingOwner(value, platform);
    if (defaultClash && defaultClash !== action) {
      errors.push([action, `conflicts-default:${defaultClash}`]);
      continue;
    }
    taken.set(key, action);
  }
  return errors;
}

function bindingKey(binding: KeyBinding): string {
  return `${binding.ctrl ? "C" : ""}${binding.meta ? "M" : ""}${binding.shift ? "S" : ""}:${binding.code}`;
}

/** 이 플랫폼 기본표에서 그 조합을 쓰는 액션(있으면). */
function defaultBindingOwner(binding: KeyBinding, platform: Platform): ShortcutAction | null {
  for (const action of OVERRIDABLE_ACTIONS) {
    for (const code of candidateCodes(action, platform)) {
      const candidate = defaultBindingFor(action, code, platform);
      if (candidate && sameBinding(candidate, binding)) return action;
    }
  }
  return null;
}

/** 액션이 플랫폼 기본표에서 사용할 수 있는 물리 code 후보(넘패드 포함). */
function candidateCodes(action: ShortcutAction, platform: Platform): string[] {
  switch (action) {
    case "split-row":
      return ["KeyD"];
    case "split-column":
      // darwin은 Cmd+Shift+D, 그 외는 Ctrl+Shift+E — 둘 다 후보로 검사한다.
      return platform === "darwin" ? ["KeyD"] : ["KeyE"];
    case "close-pane":
      return ["KeyW"];
    case "palette":
      return ["KeyP"];
    case "queue-toggle":
    case "broadcast-toggle":
      return ["KeyB"];
    case "layout-editor":
      return ["KeyG"];
    case "copy":
      return ["KeyC"];
    case "paste":
      return ["KeyV"];
    case "search":
      return ["KeyF"];
    case "zoom-in":
      return ["Equal", "NumpadAdd"];
    case "zoom-out":
      return ["Minus", "NumpadSubtract"];
    case "zoom-reset":
      return ["Digit0", "Numpad0"];
    case "next-tab":
      return platform === "darwin" ? ["BracketRight"] : ["PageDown"];
    case "prev-tab":
      return platform === "darwin" ? ["BracketLeft"] : ["PageUp"];
    case "new-mission":
      return ["KeyM"];
    case "resume-all":
      return ["KeyR"];
    default:
      return [];
  }
}

/** (action, code)의 기본 수정키 조합(플랫폼별). */
function defaultBindingFor(
  action: ShortcutAction,
  code: string,
  platform: Platform,
): KeyBinding | null {
  const mac = platform === "darwin";
  switch (action) {
    case "split-row":
      return { code: "KeyD", ctrl: !mac, meta: mac, shift: !mac };
    case "split-column":
      return mac
        ? { code: "KeyD", ctrl: false, meta: true, shift: true }
        : { code: "KeyE", ctrl: true, meta: false, shift: true };
    case "close-pane":
      return { code: "KeyW", ctrl: !mac, meta: mac, shift: !mac };
    case "palette":
      return { code: "KeyP", ctrl: !mac, meta: mac, shift: true };
    case "queue-toggle":
      return { code: "KeyB", ctrl: !mac, meta: mac, shift: false };
    case "broadcast-toggle":
      return { code: "KeyB", ctrl: !mac, meta: mac, shift: true };
    case "layout-editor":
      return { code: "KeyG", ctrl: !mac, meta: mac, shift: true };
    case "copy":
      return mac
        ? { code: "KeyC", ctrl: false, meta: true, shift: false }
        : { code: "KeyC", ctrl: true, meta: false, shift: true };
    case "paste":
      return mac
        ? { code: "KeyV", ctrl: false, meta: true, shift: false }
        : { code: "KeyV", ctrl: true, meta: false, shift: true };
    case "search":
      return mac
        ? { code: "KeyF", ctrl: false, meta: true, shift: false }
        : { code: "KeyF", ctrl: true, meta: false, shift: true };
    case "zoom-in":
      return code === "NumpadAdd"
        ? { code: "NumpadAdd", ctrl: !mac, meta: mac, shift: false }
        : { code: "Equal", ctrl: !mac, meta: mac, shift: false };
    case "zoom-out":
      return code === "NumpadSubtract"
        ? { code: "NumpadSubtract", ctrl: !mac, meta: mac, shift: false }
        : { code: "Minus", ctrl: !mac, meta: mac, shift: false };
    case "zoom-reset":
      return code === "Numpad0"
        ? { code: "Numpad0", ctrl: !mac, meta: mac, shift: false }
        : { code: "Digit0", ctrl: !mac, meta: mac, shift: false };
    // 탭 순환: macOS는 Cmd+Shift+[ ·](Safari·터미널 관행), 그 외는 맨
    // Ctrl+PageUp/PageDown(Windows Terminal·GNOME 터미널 관행).
    case "next-tab":
      return mac
        ? { code: "BracketRight", ctrl: false, meta: true, shift: true }
        : { code: "PageDown", ctrl: true, meta: false, shift: false };
    case "prev-tab":
      return mac
        ? { code: "BracketLeft", ctrl: false, meta: true, shift: true }
        : { code: "PageUp", ctrl: true, meta: false, shift: false };
    case "new-mission":
      return { code: "KeyM", ctrl: !mac, meta: mac, shift: true };
    case "resume-all":
      return { code: "KeyR", ctrl: !mac, meta: mac, shift: true };
    default:
      return null;
  }
}

/** 바인딩 표시 라벨(설정표·토스트용). meta 키 이름은 플랫폼을 따른다. */
export function bindingLabel(binding: KeyBinding, platform: Platform = currentPlatform()): string {
  const parts: string[] = [];
  if (binding.ctrl) parts.push("Ctrl");
  if (binding.meta) parts.push(platform === "darwin" ? "Cmd" : platform === "windows" ? "Win" : "Super");
  if (binding.shift) parts.push("Shift");
  parts.push(binding.code.replace(/^Key/, "").replace(/^Digit/, ""));
  return parts.join("+");
}

/**
 * 메뉴·툴팁이 보이는 그 액션의 조합: 사용자 재정의가 있으면 그것, 없으면 기본표의 첫 조합.
 * "pass"(터미널로 돌려보냄)면 null — 앱이 그 조합을 갖지 않는다.
 */
export function effectiveBinding(
  action: ShortcutAction,
  platform: Platform,
  overrides?: ShortcutOverrides,
): KeyBinding | null {
  const override = overrides?.[action];
  if (override === "pass") return null;
  return override ?? firstDefaultBinding(action, platform);
}

/** 메뉴·툴팁용 단축키 표시. 사용자 재정의를 반영하고, "pass"면 null. */
export function shortcutHint(
  action: ShortcutAction,
  platform: Platform,
  overrides?: ShortcutOverrides,
): string | null {
  const binding = effectiveBinding(action, platform, overrides);
  return binding ? hintLabel(binding, platform) : null;
}

/** 기본표에서 그 액션의 첫 후보 code 조합(넘패드 대체 키는 표시하지 않는다). */
function firstDefaultBinding(action: ShortcutAction, platform: Platform): KeyBinding | null {
  const code = candidateCodes(action, platform)[0];
  return code ? defaultBindingFor(action, code, platform) : null;
}

const HINT_KEY_NAMES = new Map<string, string>([
  ["BracketLeft", "["],
  ["BracketRight", "]"],
  ["PageUp", "PgUp"],
  ["PageDown", "PgDn"],
  ["Equal", "="],
  ["Minus", "-"],
  ["NumpadAdd", "+"],
  ["NumpadSubtract", "-"],
  ["Numpad0", "0"],
]);

function hintKeyName(code: string): string {
  const named = HINT_KEY_NAMES.get(code);
  if (named !== undefined) return named;
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  return code;
}

function hintLabel(binding: KeyBinding, platform: Platform): string {
  const key = hintKeyName(binding.code);
  if (platform === "darwin") {
    // macOS 메뉴 표기: ⌃ ⇧ ⌘ 순서로 붙여 쓴다(Alt는 터미널 Meta라 바인딩에 없다).
    return `${binding.ctrl ? "⌃" : ""}${binding.shift ? "⇧" : ""}${binding.meta ? "⌘" : ""}${key}`;
  }
  const parts: string[] = [];
  if (binding.ctrl) parts.push("Ctrl");
  if (binding.meta) parts.push(platform === "windows" ? "Win" : "Super");
  if (binding.shift) parts.push("Shift");
  parts.push(key);
  return parts.join("+");
}

export function detectPlatform(userAgent: string, platformString: string): Platform {
  const ua = `${userAgent} ${platformString}`.toLowerCase();
  if (ua.includes("win")) return "windows";
  if (ua.includes("mac") || ua.includes("darwin")) return "darwin";
  return "linux";
}

/** 실행 중인 브라우저의 플랫폼(UA 판정). navigator가 없으면(node) linux. */
export function currentPlatform(): Platform {
  if (typeof navigator === "undefined") return "linux";
  const nav = navigator as { userAgent?: string; platform?: string };
  return detectPlatform(nav.userAgent ?? "", nav.platform ?? "");
}

function baseEvent(): KeyEventLike {
  return {
    key: "d",
    code: "KeyD",
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    repeat: false,
  };
}

/** Test/dev helper: build a KeyEventLike without a DOM event. */
export function keyEvent(patch: Partial<KeyEventLike>): KeyEventLike {
  return { ...baseEvent(), ...patch };
}

/**
 * Map a keydown to an app action, or null when the key must reach the
 * terminal (or whatever currently owns focus) untouched.
 */
export function mapShortcut(event: KeyEventLike, ctx: ShortcutContext): ShortcutAction | null {
  // 조건 ①(W3-2): 가드(modal·IME·Alt·repeat 정책)는 재정의로도 우회 불가.
  if (ctx.modalOpen) return null;
  if (ctx.imeComposing) return null;
  if (event.altKey) return null;

  // 사용자 재정의 테이블: 기본표보다 먼저 정확 일치를 찾는다.
  if (ctx.overrides) {
    const override = matchOverride(event, ctx.overrides);
    if (override !== undefined) return override;
  }

  const result = defaultShortcut(event, ctx);
  // "pass" 재정의: 기본표가 그 조합을 소비하고 있다면 터미널로 돌려보낸다.
  if (result !== null && ctx.overrides?.[result] === "pass") return null;
  return result;
}

/** 기본표만 보는 판정(재정의 미적용) — mapShortcut의 본래 로직. */
function defaultShortcut(event: KeyEventLike, ctx: ShortcutContext): ShortcutAction | null {
  // Zoom은 repeat을 허용한다(누르고 있으면 연속 조정) — repeat 차단은
  // pane 생성·포커스 변경 계열에만 적용된다.
  const zoom = mapZoomShortcut(event, ctx.platform);
  if (zoom !== null) return zoom;

  if (event.repeat) return null; // 길게 누른 반복 이벤트로 연속 생성 금지

  const cmd = event.metaKey && !event.ctrlKey;
  const ctrlShift = event.ctrlKey && event.shiftKey && !event.metaKey;

  if (ctx.platform === "darwin") {
    if (!cmd) return null;
    switch (event.code) {
      case "KeyD":
        return event.shiftKey ? "split-column" : "split-row";
      case "KeyW":
        return event.shiftKey ? null : "close-pane";
      case "KeyP":
        return event.shiftKey ? "palette" : null;
      case "KeyB":
        return event.shiftKey ? "broadcast-toggle" : "queue-toggle";
      case "KeyG":
        return event.shiftKey ? "layout-editor" : null;
      case "KeyC":
        // macOS Cmd+C: selection이 없으면 no-op(터미널로도 전달하지 않음).
        return event.shiftKey ? null : ctx.hasSelection ? "copy" : null;
      case "KeyV":
        return event.shiftKey ? null : "paste";
      case "KeyF":
        // Cmd+F: 현재 pane scrollback 검색 열기. 맨 Ctrl+F는 셸의
        // forward-char(터미널 입력)이므로 darwin에서만 단독 Cmd+F.
        return event.shiftKey ? null : "search";
      case "BracketRight":
        // Cmd+Shift+] — 다음 탭. Shift 없는 Cmd+]는 터미널로 보낸다.
        return event.shiftKey ? "next-tab" : null;
      case "BracketLeft":
        return event.shiftKey ? "prev-tab" : null;
      case "KeyM":
        // Cmd+Shift+M — 새 AI 작업. 맨 Cmd+M은 시스템 최소화(창 메뉴)의 것이다.
        return event.shiftKey ? "new-mission" : null;
      case "KeyR":
        // Cmd+Shift+R — 일시정지 작업 모두 재개. 맨 Cmd+R은 앱이 가져가지 않는다.
        return event.shiftKey ? "resume-all" : null;
      default:
        return null;
    }
  }

  if (event.code === "KeyB" && event.ctrlKey && !event.metaKey && !event.shiftKey) {
    return "queue-toggle";
  }

  // 탭 순환은 맨 Ctrl+PageUp/PageDown이다(Windows Terminal·GNOME 터미널과 같다).
  // PageUp/PageDown 자체는 셸이 거의 쓰지 않고, Shift 조합(Ctrl+Shift+PageUp)은
  // 터미널 스크롤로 남겨 둔다.
  if (
    (event.code === "PageDown" || event.code === "PageUp") &&
    event.ctrlKey &&
    !event.metaKey &&
    !event.shiftKey
  ) {
    return event.code === "PageDown" ? "next-tab" : "prev-tab";
  }

  // Windows/Linux: 나머지 앱 조합은 Ctrl+Shift+<key>. 맨 Ctrl+D/C는 그대로 터미널에
  // 전달된다(Ctrl+D → 0x04 EOF, Ctrl+C → interrupt).
  if (!ctrlShift) return null;
  switch (event.code) {
    case "KeyD":
      return "split-row";
    case "KeyE":
      return "split-column";
    case "KeyW":
      return "close-pane";
    case "KeyP":
      return "palette";
    case "KeyB":
      return "broadcast-toggle";
    case "KeyG":
      return "layout-editor";
    case "KeyC":
      return "copy";
    case "KeyV":
      return "paste";
    case "KeyF":
      return "search";
    case "KeyM":
      return "new-mission";
    case "KeyR":
      return "resume-all";
    default:
      return null;
  }
}

/**
 * Zoom combos (04 §3): macOS Cmd+= / Cmd+- / Cmd+0, 그 외 Ctrl 조합.
 * `code` 기반이므로 Shift(=와 +가 같은 키), 넘패드(+/-/0), IME 키보드
 * 레이아웃 차이와 무관하게 매핑된다. 맨 =/-/0(수정키 없음)은 터미널
 * 입력으로 그대로 전달된다(null 반환).
 */
/** 이 이벤트에 정확히 일치하는 재정의가 있으면 그 액션을 반환. */
function matchOverride(
  event: KeyEventLike,
  overrides: ShortcutOverrides,
): ShortcutAction | undefined {
  for (const action of OVERRIDABLE_ACTIONS) {
    const binding = overrides[action];
    if (!binding || binding === "pass") continue;
    if (
      event.code === binding.code &&
      event.ctrlKey === binding.ctrl &&
      event.metaKey === binding.meta &&
      event.shiftKey === binding.shift
    ) {
      return action;
    }
  }
  return undefined;
}

function mapZoomShortcut(event: KeyEventLike, platform: Platform): ShortcutAction | null {
  const mod =
    platform === "darwin"
      ? event.metaKey && !event.ctrlKey
      : event.ctrlKey && !event.metaKey;
  if (!mod) return null;
  switch (event.code) {
    case "Equal":
    case "NumpadAdd":
      return "zoom-in";
    case "Minus":
    case "NumpadSubtract":
      return "zoom-out";
    case "Digit0":
    case "Numpad0":
      return "zoom-reset";
    default:
      return null;
  }
}
