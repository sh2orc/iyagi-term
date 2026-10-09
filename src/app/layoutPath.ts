/** 경로의 뒤쪽 두 조각만(블록·목록 폭이 좁다). 전체 경로는 툴팁에 있다. */
export function tailPath(path: string): string {
  const parts = path.split(/[\\/]+/).filter(Boolean);
  return parts.length <= 2 ? path : `…/${parts.slice(-2).join("/")}`;
}
