/**
 * 탭 막대 스크롤 수학(04-ui §1 상단 바). 활성 탭이 막대의 보이는 창 밖에
 * 있으면 그 탭을 화면 안으로 들어오게 하는 최소 scrollLeft를 구한다.
 * 보이는 중이면 null — 터치패드로 밀어 둔 위치를 흔들지 않기 위해서다.
 */

/** 탭을 창 가장자리에 딱 붙이지 않고 남겨 두는 여백(px). */
const REVEAL_MARGIN_PX = 6;

/** 막대의 스크롤 상태와 탭의 위치·폭 — DOM 직접 참조 대신 값만 받는다. */
export interface ScrollBox {
  scrollLeft: number;
  clientWidth: number;
}

export interface TabBox {
  offsetLeft: number;
  offsetWidth: number;
}

/**
 * 활성 탭을 보이게 하는 목표 scrollLeft. 이미 보이면 null을 돌려준다.
 * 오른쪽으로 밀려 나갔으면 탭의 오른쪽 끝이 창 안에 들어오게, 왼쪽으로
 * 밀려 나갔으면 왼쪽 끝에 여백을 두고 맞춘다(0 미만은 0으로).
 */
export function revealTabScroll(bar: ScrollBox, tab: TabBox): number | null {
  const left = tab.offsetLeft;
  const right = left + tab.offsetWidth;
  const viewLeft = bar.scrollLeft;
  const viewRight = viewLeft + bar.clientWidth;
  if (right > viewRight) {
    return right - bar.clientWidth + REVEAL_MARGIN_PX;
  }
  if (left < viewLeft) {
    return Math.max(0, left - REVEAL_MARGIN_PX);
  }
  return null;
}
