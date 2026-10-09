/**
 * 파일을 터미널 위에 끌어다 놓으면 그 경로를 붙여넣는다 — 터미널의 오랜
 * 관례이자, 이미지·로그를 에이전트에게 건네는 가장 빠른 길이다.
 *
 * Tauri 창은 `dragDropEnabled` 기본값이 켜짐이라 웹뷰에는 HTML5 drop 이벤트가
 * 오지 않는다. 경로와 좌표는 `tauri://drag-drop`에서 오고(app/fileDrop.ts),
 * 이 모듈은 그 좌표를 pane으로 되짚고 경로를 셸이 한 낱말로 읽는 문자열로
 * 만든다. DOM 밖의 판정은 전부 순수 함수라 시험에서 그대로 돌릴 수 있다.
 */

import { terminalLeafIdOf } from "./nativePaste";
import type { Platform } from "./shortcuts";

/** 따옴표 없이 한 낱말로 남는 POSIX 경로 글자(물결표는 앞에서 확장되므로 제외). */
const POSIX_BARE = /^[A-Za-z0-9._\-+=:,@%/]+$/;
/** cmd·PowerShell이 낱말을 끊거나 다르게 읽는 글자. */
const WINDOWS_NEEDS_QUOTE = /[\s&()[\]{}^=;!'+,`~]/;

/**
 * 한 경로를 붙여넣기 가능한 형태로 만든다. Windows 경로에는 `"`가 올 수
 * 없으므로(파일 이름 금지 문자) 겹따옴표로 감싸기만 하고, 혹시 섞여 들어온
 * 것은 떼어 낸다. POSIX는 홑따옴표 + `'\''` 탈출로 무엇이든 감쌀 수 있다.
 */
export function quoteDropPath(path: string, platform: Platform): string {
  if (platform === "windows") {
    const bare = path.replace(/"/g, "");
    return WINDOWS_NEEDS_QUOTE.test(bare) ? `"${bare}"` : bare;
  }
  if (POSIX_BARE.test(path)) return path;
  return `'${path.replace(/'/g, "'\\''")}'`;
}

/** 떨어뜨린 경로 여럿 → 공백으로 이은 한 줄(빈 경로는 버린다). */
export function dropPayload(paths: readonly string[], platform: Platform): string {
  return paths
    .filter((path) => path.length > 0)
    .map((path) => quoteDropPath(path, platform))
    .join(" ");
}

/**
 * 네이티브 drag-drop 좌표는 물리 픽셀이다 — `elementFromPoint`가 쓰는 CSS
 * 픽셀로 바꾼다. scale이 0·NaN이면(창이 아직 없는 경우) 1로 본다.
 */
export function toCssPoint(
  position: { x: number; y: number },
  scale: number,
): { x: number; y: number } {
  const ratio = Number.isFinite(scale) && scale > 0 ? scale : 1;
  return { x: position.x / ratio, y: position.y / ratio };
}

/** 그 자리에 있는 터미널 pane의 leaf id(터미널 밖이면 null). */
export function leafIdAtPoint(x: number, y: number, doc: Document): string | null {
  return terminalLeafIdOf(doc.elementFromPoint(x, y));
}

/** 끄는 동안 대상 pane만 테두리로 표시한다(null이면 전부 지운다). */
export function markDropTarget(leafId: string | null, doc: Document): void {
  for (const pane of Array.from(doc.querySelectorAll<HTMLElement>("[data-leaf-id]"))) {
    pane.classList.toggle("drop-target", leafId !== null && pane.dataset.leafId === leafId);
  }
}
