/**
 * 스크롤백 메모리 어림(설정 → 터미널 → 스크롤백 줄 수의 안내).
 *
 * xterm 버퍼는 셀마다 글자·속성을 담아 셀당 대략 12바이트를 쓴다. pane의 실제 폭은
 * 창마다 다르므로 넓은 창(200열)을 가정해 넉넉히 어림한다 — 정확한 값이 아니라
 * "줄 수를 크게 잡으면 pane마다 이만큼 든다"는 감을 주는 것이 목적이다.
 */

export const SCROLLBACK_BYTES_PER_CELL = 12;
export const SCROLLBACK_ASSUMED_COLS = 200;
/** 이보다 많으면 안내를 경고로 바꾼다. */
export const SCROLLBACK_WARN_LINES = 20_000;

/** pane 하나의 스크롤백이 차지할 메모리 어림(MB, 10 MB 미만은 소수 첫째 자리까지). */
export function scrollbackMemoryMB(lines: number): number {
  const safeLines = Number.isFinite(lines) && lines > 0 ? lines : 0;
  const mb = (safeLines * SCROLLBACK_ASSUMED_COLS * SCROLLBACK_BYTES_PER_CELL) / (1024 * 1024);
  return mb < 10 ? Math.round(mb * 10) / 10 : Math.round(mb);
}

/** 경고할 만큼 큰가(2만 줄 초과). */
export function scrollbackNeedsWarning(lines: number): boolean {
  return lines > SCROLLBACK_WARN_LINES;
}
