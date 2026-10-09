/**
 * URL 처리 보안 규칙 (04-ui.md §6):
 * - terminal output의 HTML을 DOM으로 해석하지 않는다.
 * - URL 인식은 https/http만 허용하고 사용자 클릭으로만 OS opener를 쓴다.
 * - scheme allowlist 밖이면 아무 동작도 하지 않는다.
 */

import { isTauri as detectTauri, tauriIpcAdapter } from "../bridge/ipc";

const ALLOWED_SCHEMES = new Set(["https:", "http:"]);

/** 시험에서 Tauri 판정과 invoke를 대체하기 위한 주입점. */
export interface OpenExternalDeps {
  isTauri?: () => boolean;
  invoke?: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
}

/** https/http가 아니면 null. */
export function sanitizeUrl(raw: string): string | null {
  const trimmed = raw.trim();
  if (!/^https?:\/\//i.test(trimmed)) return null;
  try {
    const url = new URL(trimmed);
    if (!ALLOWED_SCHEMES.has(url.protocol)) return null;
    return url.toString();
  } catch {
    return null;
  }
}

/**
 * OS opener로 연다. Tauri 웹뷰에서는 shell 플러그인의 `open` 명령을
 * invoke로 직접 부른다(withGlobalTauri를 켜지 않으므로 `__TAURI__` 전역은
 * 없고, WKWebView/WebView2는 window.open을 무시한다). capabilities의
 * `shell:allow-open`과 플러그인 기본 scope 정규식이 http(s)를 허용한다.
 * 브라우저 미리보기에서는 window.open noopener로 처리한다.
 */
export async function openExternal(raw: string, deps: OpenExternalDeps = {}): Promise<boolean> {
  const safe = sanitizeUrl(raw);
  if (!safe) return false;
  if ((deps.isTauri ?? detectTauri)()) {
    try {
      await (deps.invoke ?? tauriIpcAdapter.invoke)("plugin:shell|open", { path: safe });
      return true;
    } catch {
      // 웹뷰는 window.open을 무시하므로 여기서 실패하면 열 수단이 없다.
      return false;
    }
  }
  try {
    const win = globalThis as { open?: (url: string, target: string, features: string) => unknown };
    win.open?.(safe, "_blank", "noopener,noreferrer");
    return true;
  } catch {
    return false;
  }
}

/** https/http URL 후보 추출(링크 provider용). */
export function findUrls(text: string): Array<{ start: number; end: number; url: string }> {
  const results: Array<{ start: number; end: number; url: string }> = [];
  const pattern = /https?:\/\/[^\s'"<>)]+/gi;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(text)) !== null) {
    const url = sanitizeUrl(match[0]);
    if (url) results.push({ start: match.index, end: match.index + match[0].length, url });
  }
  return results;
}
