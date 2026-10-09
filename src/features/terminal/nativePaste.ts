/**
 * 네이티브 붙여넣기 경로(04-ui §6 붙여넣기).
 *
 * `navigator.clipboard.readText()`로 읽으면 WebKit(macOS WKWebView)은 다른
 * 앱이 복사한 내용일 때마다 "붙여넣기" 확인 말풍선을 띄워 한 번 더 누르게
 * 한다. 반면 웹뷰가 스스로 실행한 붙여넣기 명령은 신뢰된 `paste` 이벤트에
 * 클립보드 텍스트를 실어 보내므로 확인이 없다. 그래서 기본 붙여넣기 조합은
 * keydown을 막지 않고 뒤따르는 paste 이벤트에서 텍스트를 받는다
 * (Workbench의 onPaste → SessionController.pasteText).
 *
 * 웹뷰가 붙여넣기 명령으로 바꾸는 조합:
 * - macOS: Cmd+V — 앱 메뉴(편집 › 붙여넣기)의 단축키. 막히지 않은 keydown을
 *   WKWebView가 메뉴로 넘기고, 메뉴가 `paste:`를 실행한다.
 * - Windows(WebView2): Ctrl+Shift+V — Chromium 편집 명령 PasteAndMatchStyle.
 * - Linux(WebKitGTK): Ctrl+Shift+V — 키 바인딩 PasteAsPlainText.
 */
import type { KeyEventLike, Platform } from "./shortcuts";

/**
 * 웹뷰가 스스로 붙여넣기를 실행하는 기본 조합인가. 웹뷰는 조합을 글쇠 문자로
 * 맞추므로, 물리 V 자리가 다른 라틴 글자를 내는 배열(Dvorak 등)은 아니다 —
 * 그때는 앱이 클립보드를 직접 읽는다. 한글 같은 비라틴 문자는 OS가 단축키를
 * 영문 배열로 맞추므로 V로 본다.
 */
export function isNativePasteKey(event: KeyEventLike, platform: Platform): boolean {
  if (event.code !== "KeyV" || event.altKey) return false;
  if (/^[a-z]$/i.test(event.key) && event.key.toLowerCase() !== "v") return false;
  return platform === "darwin"
    ? event.metaKey && !event.ctrlKey && !event.shiftKey
    : event.ctrlKey && event.shiftKey && !event.metaKey;
}

/** `closest`만 쓰는 DOM 요소 표면 — node 테스트는 가짜로 대신한다. */
interface ClosestLookup {
  closest(selector: string): { getAttribute(name: string): string | null } | null;
}

function hasClosest(value: unknown): value is ClosestLookup {
  return typeof (value as { closest?: unknown } | null)?.closest === "function";
}

/**
 * 이벤트 대상이 터미널(xterm) 안이면 그 pane의 leafId. 입력 칸·메뉴처럼
 * xterm 밖이면 null — 그 붙여넣기는 브라우저 기본 동작에 맡긴다.
 */
export function terminalLeafIdOf(target: EventTarget | null): string | null {
  if (!hasClosest(target)) return null;
  if (target.closest(".xterm") === null) return null;
  return target.closest("[data-leaf-id]")?.getAttribute("data-leaf-id") ?? null;
}
