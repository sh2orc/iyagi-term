/**
 * 탭 전환 시각 효과의 순수 부분(방향 판정).
 *
 * 구조상 이전 탭은 즉시 언마운트되므로(Workbench는 활성 탭만 그린다) 두 탭을
 * 겹쳐 크로스페이드할 수 없다. 그래서 들어오는 탭만 진행 방향에서 살짝 슬라이드
 * +페이드로 나타낸다. 이 파일은 "어느 방향인가"만 정하고(탭 인덱스 변화), 실제
 * 애니메이션은 workbench.css의 keyframes가, 재시작은 Workbench의 효과가 맡는다.
 */

export type TabSwitchDirection = "next" | "prev";

/** 전환 애니메이션 길이(ms). CSS와 맞춘다(빠릿한 확인 신호, 스와이프 방향 일치). */
export const TAB_SWITCH_DURATION_MS = 150;

/**
 * 탭 인덱스 변화 → 슬라이드 방향.
 *
 * - 오른쪽(더 큰 인덱스)으로 가면 `"next"`(오른쪽에서 들어옴),
 * - 왼쪽이면 `"prev"`,
 * - 같거나 첫 렌더(prevIndex === null)이거나 대상을 못 찾으면(nextIndex < 0) null.
 *   null이면 애니메이션을 걸지 않는다 — 첫 화면에서 번쩍이지 않게.
 */
export function tabSwitchDirection(
  prevIndex: number | null,
  nextIndex: number,
): TabSwitchDirection | null {
  if (prevIndex === null || nextIndex < 0 || nextIndex === prevIndex) return null;
  return nextIndex > prevIndex ? "next" : "prev";
}
