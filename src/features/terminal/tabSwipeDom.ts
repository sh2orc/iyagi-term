/**
 * 트랙패드 탭 전환 제스처의 DOM 층 — 인식기(tabSwipe.ts)를 워크벤치 루트에 붙인다.
 *
 * capture 단계의 non-passive wheel 리스너 하나만 쓴다. capture라야 xterm의
 * 자체 wheel 처리보다 먼저 보고, non-passive라야 preventDefault로 가로 제스처를
 * 가져올 수 있다. 소비는 가로 제스처일 때뿐이고, 세로 스크롤은 손대지 않는다
 * (터미널 스크롤·목록 스크롤은 그대로 동작해야 한다).
 *
 * 먹이기 전에 두 가지를 확인한다.
 *  ① 설정이 꺼져 있으면 아무 일도 하지 않는다.
 *  ② 이벤트 대상에서 루트까지의 조상 중 가로로 스크롤되는 요소가 있으면
 *     그 스크롤이 임자다(탭 바가 넘칠 때·가로 스크롤되는 목록 위).
 *
 * 터미널 위에서도 동작한다. 마우스 보고 중인 TUI(Claude·Codex 등)라도 가로
 * 휠은 앱이 쓰는 일이 거의 없으므로 가로 우위 제스처만 가로채 탭을 넘긴다.
 * 세로 스크롤은 인식기가 소비하지 않으므로(그대로 흘려보낸다) 터미널·목록
 * 스크롤은 mouse tracking 여부와 무관하게 온전히 앱에 전달된다.
 */

import {
  TabSwipeRecognizer,
  tabSwipeStep,
  type TabSwipeDirection,
  type TabSwipePrefs,
} from "./tabSwipe";

export interface TabSwipeDeps {
  /** 현재 설정(이벤트마다 읽는다 — 설정 변경이 곧바로 반영된다). */
  prefs(): TabSwipePrefs;
  /** 탭 전환. 실제로 바뀌었으면 true. */
  cycle(direction: TabSwipeDirection): boolean;
}

const ELEMENT_NODE = 1;

export function attachTabSwipe(root: HTMLElement, deps: TabSwipeDeps): () => void {
  const recognizer = new TabSwipeRecognizer();

  const onWheel = (event: WheelEvent): void => {
    const prefs = deps.prefs();
    if (!prefs.enabled) {
      recognizer.reset();
      return;
    }
    const target = elementOf(event.target);
    if (target !== null && scrollsHorizontally(target, root)) {
      // 진짜 가로 스크롤되는 조상(탭 바 넘침·가로 목록)이 임자다.
      recognizer.reset();
      return;
    }
    recognizer.configure({
      stepPx: tabSwipeStep(prefs.sensitivity),
      reverse: prefs.reverse,
    });
    const direction = recognizer.feed({
      deltaX: event.deltaX,
      deltaY: event.deltaY,
      deltaMode: event.deltaMode,
      timeStamp: event.timeStamp,
    });
    if (recognizer.consumedLastEvent) {
      // 관성 꼬리까지 막는다 — 그러지 않으면 한 번의 스와이프가 xterm 쪽에
      // 가로 스크롤/마우스 보고로 새어 나간다.
      event.preventDefault();
      event.stopPropagation();
    }
    if (direction !== null) deps.cycle(direction);
  };

  root.addEventListener("wheel", onWheel, { capture: true, passive: false });
  return () => root.removeEventListener("wheel", onWheel, true);
}

/** 이벤트 대상이 요소면 그 요소(텍스트 노드·window면 null). */
function elementOf(target: EventTarget | null): Element | null {
  const node = target as Node | null;
  if (node === null || typeof node !== "object" || node.nodeType !== ELEMENT_NODE) return null;
  return node as Element;
}

/** 대상~루트 사이(루트 제외)에 가로로 스크롤되는 조상이 있는가. */
export function scrollsHorizontally(target: Element, root: Element): boolean {
  let node: Element | null = target;
  while (node !== null && node !== root) {
    if (isHorizontallyScrollable(node)) return true;
    node = node.parentElement;
  }
  return false;
}

function isHorizontallyScrollable(element: Element): boolean {
  // 1px 여유: 소수 픽셀 레이아웃에서 scrollWidth가 clientWidth를 아주 조금
  // 넘는 일이 흔하다(스크롤은 실제로 일어나지 않는다).
  if (element.scrollWidth <= element.clientWidth + 1) return false;
  try {
    const view = element.ownerDocument?.defaultView ?? null;
    const overflowX = view?.getComputedStyle(element).overflowX;
    return overflowX === "auto" || overflowX === "scroll";
  } catch {
    return false;
  }
}
