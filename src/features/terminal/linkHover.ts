/**
 * 마우스 아래에 있는 링크(터미널별).
 *
 * xterm 링크의 hover/leave가 채우고, 컨텍스트 메뉴는 이 값이 있을 때만
 * "링크 열기·링크 주소 복사"를 보인다 — 오른쪽 클릭 순간 마우스는 그 링크
 * 위에 있으므로 좌표→셀 변환(xterm 비공개 API) 없이 알 수 있다.
 * WeakMap이라 터미널을 버리면 함께 사라진다.
 */

const hovered = new WeakMap<object, string>();

export function setHoveredLink(terminal: object, url: string | null): void {
  if (url) hovered.set(terminal, url);
  else hovered.delete(terminal);
}

export function hoveredLink(terminal: object): string | null {
  return hovered.get(terminal) ?? null;
}
