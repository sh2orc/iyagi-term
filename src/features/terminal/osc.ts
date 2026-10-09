/**
 * OSC 파싱 순수 함수(W1-3/4) — 브라우저 전용 xtermSetup에서 분리해
 * node 시험이 가능하게 했다.
 *
 * - 제목(OSC 0/2): 제어 문자 제거·트림·200자 상한. 빈 값은 null.
 * - cwd(OSC 7): `file://host/path`에서 경로만 쓴다(로컬 셸이 자기 cwd를
 *   알려준 것). percent-decoding하고, Windows 드라이브 문자 형태
 *   (`file:///C:/Users/...` → pathname `/C:/...`)의 앞 슬래시를 덜어낸다.
 */

import { isAbsolutePath } from "../profiles/validation";
import type { Platform } from "./shortcuts";

export const MAX_TITLE_LEN = 200;
const MAX_CWD_LEN = 4096;

/**
 * 셸이 보고한 cwd를 "다음 pane의 시작 cwd"로 써도 되는 형태로 다듬는다.
 * 실행 플랫폼의 절대 경로만 받는다 — Windows에서 WSL·Git Bash가 보고한
 * POSIX 경로(`/home/u`)는 PowerShell/cmd의 cwd가 될 수 없고 데몬도
 * INVALID_ARGUMENT로 거절하므로 버린다(null). 드라이브 경로는 `\\`로 통일.
 */
export function acceptReportedCwd(cwd: string, platform: Platform): string | null {
  if (platform === "windows") {
    if (!isAbsolutePath(cwd, "windows")) return null;
    return cwd.replace(/\//g, "\\");
  }
  return isAbsolutePath(cwd, "other") ? cwd : null;
}

export function sanitizeOscTitle(raw: string): string | null {
  const title = raw.replace(/[\x00-\x1f\x7f]/g, "").trim();
  return title.length > 0 ? title.slice(0, MAX_TITLE_LEN) : null;
}

/**
 * 저장용 제목. 에이전트 CLI는 작업 중 제목 맨 앞 점자 스피너(⠋⠙⠹…)를 프레임마다
 * 바꿔 보낸다. 그대로 저장하면 값이 초당 여러 번 달라져 LocalStorage(WAL)가 GB 단위로
 * 불어나므로, 저장 직렬화에서는 맨 앞 스피너 글리프를 뗀다(화면 제목은 그대로).
 */
export function persistedTitle<T extends string | null | undefined>(title: T): T | string {
  if (typeof title !== "string") return title;
  const stripped = title.replace(/^[\u2800-\u28ff]+\s*/, "");
  return stripped.length > 0 ? stripped : title;
}

export function cwdFromOsc7(data: string): string | null {
  try {
    const url = new URL(data);
    if (url.protocol !== "file:") return null;
    let path = decodeURIComponent(url.pathname);
    // file:///C:/… 의 pathname은 "/C:/…"로 온다 — 드라이브 문자 경로의
    // 앞 슬래시를 덜어 Windows 경로 형태를 맞춘다.
    path = path.replace(/^\/([A-Za-z]:\/.*)$/, "$1");
    path = path.replace(/\/+$/, "") || "/";
    if (path.length === 0 || path.length > MAX_CWD_LEN) return null;
    return path;
  } catch {
    return null;
  }
}
