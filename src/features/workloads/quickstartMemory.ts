/**
 * 퀵스타트 기억(W3-4 온보딩 최소): 마지막으로 1-클릭 실행한 CLI 프로그램
 * 경로만 저장한다. 프로필·설정은 건드리지 않는다 — 목록 정렬 순서와
 * "마지막 사용" 표시가 전부다.
 */

const KEY = "iyagi.quickstart.v1";
const MAX_LEN = 512;

export function getLastQuickstart(): string | null {
  try {
    const value = localStorage.getItem(KEY);
    if (typeof value !== "string" || value.length === 0 || value.length > MAX_LEN) return null;
    return value;
  } catch {
    return null;
  }
}

export function rememberQuickstart(program: string): void {
  try {
    if (program.length > 0 && program.length <= MAX_LEN) localStorage.setItem(KEY, program);
  } catch {
    // 저장 실패는 정렬 순서 기본값으로 충분하다.
  }
}
