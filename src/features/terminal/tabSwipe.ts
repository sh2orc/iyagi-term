/**
 * 트랙패드 두 손가락 가로 스와이프 → 탭 전환(제스처 인식기).
 *
 * 이 파일은 DOM을 모른다 — wheel 이벤트에서 뽑은 숫자 네 개(deltaX·deltaY·
 * deltaMode·timeStamp)만 먹고 "다음/이전 탭" 또는 null을 돌려준다. 붙이는
 * 일(리스너·스크롤 가능한 조상 판정)은 tabSwipeDom.ts가 맡는다. 그래서 인식
 * 규칙은 브라우저 없이 시험할 수 있다.
 *
 * 한 번의 스와이프는 인접 탭 하나만 넘긴다. 발동 뒤에는 관성 이벤트까지
 * 소비한다. 관성이 줄다가 다시 강해지거나 방향을 바꾸면 새 스와이프로 받는다.
 * wheel에는 손가락 접촉 정보가 없으므로 이동량 변화로 새 입력을 추정한다.
 *
 * 규칙:
 *  - 세로 우위 이벤트는 누적을 버리고 소비하지 않는다.
 *  - 발동 전 방향이 바뀌면 이전 누적을 버린다.
 *  - 누적이 step을 넘으면 한 번 발동하고 제스처가 끝날 때까지 잠근다.
 *  - 세로 이벤트는 잠금을 풀지 않는다. 작은 관성 흔들림도 무시한다.
 *  - deltaMode: 0 = 픽셀(트랙패드), 1 = 줄(휠) → 16px로 환산, 2 = 페이지 → 무시.
 *
 * `consumedLastEvent`는 방금 먹인 이벤트를 DOM 층이 preventDefault·
 * stopPropagation 해야 하는지 알려 준다. 가로 제스처가 진행 중(누적이 데드존을
 * 넘었다)이면 참이고, 세로 이벤트에서는 절대 참이 아니다.
 */

/** 설정의 감도 — step(px)만 다르다. */
export type TabSwipeSensitivity = "low" | "medium" | "high";

export const TAB_SWIPE_SENSITIVITIES: readonly TabSwipeSensitivity[] = ["low", "medium", "high"];

/**
 * 감도 → 한 탭 step(px). 한 손가락 마디 정도의 스와이프가 macOS에서 대략
 * 80~160px의 deltaX를 만든다 — 보통은 가볍게 밀면 한 탭, 낮음은 좀 더 밀어야
 * 넘고, 높음은 아주 살짝만 밀어도 넘는 값이다. 값이 작을수록 빠릿하다.
 */
export const TAB_SWIPE_STEP_PX: Readonly<Record<TabSwipeSensitivity, number>> = {
  low: 130,
  medium: 78,
  high: 48,
};

/** 이 이상 이벤트가 끊기면 손을 뗀 것으로 보고 제스처를 새로 시작한다(ms). */
export const TAB_SWIPE_GESTURE_RESET_MS = 200;

/** 이 이상 가로로 밀렸으면 "제스처 중"으로 보고 이벤트를 소비한다(px). */
export const TAB_SWIPE_DEAD_ZONE_PX = 8;

/** deltaMode === 1(줄 단위 휠)을 픽셀로 바꾸는 환산 계수. */
export const TAB_SWIPE_LINE_HEIGHT_PX = 16;

/** 세로 우위 판정 계수: |deltaX| < 2·|deltaY|면 세로 스크롤이다. */
export const TAB_SWIPE_VERTICAL_DOMINANCE = 2;

const DELTA_MODE_LINE = 1;
const DELTA_MODE_PAGE = 2;

/** 설정 페이지가 소유하는 값(store/preferences.ts의 `tabSwipe`). */
export interface TabSwipePrefs {
  enabled: boolean;
  sensitivity: TabSwipeSensitivity;
  /** 자연스러운 스크롤을 끈 사용자를 위한 방향 반전. */
  reverse: boolean;
  /** 끝 탭에서 반대편 끝으로 순환. */
  wrap: boolean;
}

export const DEFAULT_TAB_SWIPE: TabSwipePrefs = {
  enabled: true,
  sensitivity: "medium",
  reverse: false,
  wrap: false,
};

export type TabSwipeDirection = "next" | "prev";

export interface TabSwipeOptions {
  /** 한 탭 넘기는 데 필요한 가로 누적(px). */
  stepPx: number;
  /** 손을 뗀 것으로 보는 이벤트 간격(ms). */
  gestureResetMs: number;
  reverse: boolean;
}

/** wheel 이벤트에서 인식기가 쓰는 부분(WheelEvent를 그대로 넣어도 된다). */
export interface TabSwipeWheelLike {
  deltaX: number;
  deltaY: number;
  deltaMode: number;
  timeStamp: number;
}

/** 감도 → step(px). 모르는 값은 보통으로 본다. */
export function tabSwipeStep(sensitivity: TabSwipeSensitivity): number {
  return TAB_SWIPE_STEP_PX[sensitivity] ?? TAB_SWIPE_STEP_PX.medium;
}

function signOf(value: number): number {
  return value > 0 ? 1 : value < 0 ? -1 : 0;
}

export class TabSwipeRecognizer {
  private options: TabSwipeOptions;
  /** 지금 제스처의 가로 이동 누적(px). 발동·방향 전환·세로 우위에서 갱신. */
  private accumulated = 0;
  /** 관성 꼬리를 포함해 같은 제스처에서는 한 번만 발동한다. */
  private fired = false;
  private firedDirection = 0;
  private peakDelta = 0;
  private tailDelta: number | null = null;
  /** 직전 이벤트 시각(없으면 null — 첫 이벤트는 언제나 새 제스처다). */
  private lastEventAt: number | null = null;
  private consumed = false;

  constructor(options: Partial<TabSwipeOptions> = {}) {
    this.options = {
      stepPx: TAB_SWIPE_STEP_PX.medium,
      gestureResetMs: TAB_SWIPE_GESTURE_RESET_MS,
      reverse: false,
      ...options,
    };
  }

  /** 설정이 바뀌면 진행 중인 제스처를 깨지 않고 값만 갈아 끼운다. */
  configure(options: Partial<TabSwipeOptions>): void {
    this.options = { ...this.options, ...options };
  }

  /** 방금 먹인 이벤트를 DOM 층이 소비(preventDefault)해야 하는가. */
  get consumedLastEvent(): boolean {
    return this.consumed;
  }

  /** 제스처 상태를 버린다(설정 끄기·화면 전환 등). */
  reset(): void {
    this.accumulated = 0;
    this.fired = false;
    this.firedDirection = 0;
    this.peakDelta = 0;
    this.tailDelta = null;
    this.lastEventAt = null;
    this.consumed = false;
  }

  feed(event: TabSwipeWheelLike): TabSwipeDirection | null {
    this.consumed = false;
    // 페이지 단위(PageUp/PageDown을 흉내 내는 휠)는 트랙패드 제스처가 아니다.
    if (event.deltaMode === DELTA_MODE_PAGE) return null;

    const scale = event.deltaMode === DELTA_MODE_LINE ? TAB_SWIPE_LINE_HEIGHT_PX : 1;
    const deltaX = event.deltaX * scale;
    const deltaY = event.deltaY * scale;

    // 손을 뗀 시간은 이벤트 흐름 전체로 센다(세로 스크롤이 사이에 껴도).
    const gap =
      this.lastEventAt === null ? Number.POSITIVE_INFINITY : event.timeStamp - this.lastEventAt;
    this.lastEventAt = event.timeStamp;
    if (gap >= this.options.gestureResetMs) {
      // 입력이 충분히 끊겼으면 이동량 변화와 무관하게 새 제스처다.
      this.accumulated = 0;
      this.fired = false;
    }

    // 세로 우위 = 스크롤. 누적을 버리고 그대로 흘려보낸다(절대 소비하지 않는다).
    if (Math.abs(deltaX) < TAB_SWIPE_VERTICAL_DOMINANCE * Math.abs(deltaY)) {
      this.accumulated = 0;
      return null;
    }

    // 고정 시간마다 잠금을 풀면 긴 관성이 여러 탭을 넘긴다. 대신 감속 뒤
    // 다시 강해지는 입력이나 뚜렷한 방향 반전만 새 스와이프로 받는다.
    if (this.fired) {
      const magnitude = Math.abs(deltaX);
      const reversed =
        signOf(deltaX) !== this.firedDirection && magnitude >= TAB_SWIPE_DEAD_ZONE_PX;
      const renewed =
        this.tailDelta !== null &&
        magnitude >= Math.max(TAB_SWIPE_DEAD_ZONE_PX, this.tailDelta * 2) &&
        magnitude - this.tailDelta >= 4;
      if (reversed || (signOf(deltaX) === this.firedDirection && renewed)) {
        this.fired = false;
        this.accumulated = 0;
      } else {
        if (signOf(deltaX) === this.firedDirection) {
          this.peakDelta = Math.max(this.peakDelta, magnitude);
          if (magnitude <= this.peakDelta * 0.5) {
            this.tailDelta = Math.min(this.tailDelta ?? magnitude, magnitude);
          }
        }
        this.consumed = deltaX !== 0;
        return null;
      }
    }

    // 방향 전환: 이전 누적을 버리고 이번 방향으로 다시 센다.
    if (this.accumulated !== 0 && deltaX !== 0 && signOf(deltaX) !== signOf(this.accumulated)) {
      this.accumulated = 0;
    }
    this.accumulated += deltaX;

    if (Math.abs(this.accumulated) >= this.options.stepPx) {
      // 자연스러운 스크롤 기준: 손가락이 왼쪽으로 가면 deltaX > 0 — 종이를
      // 왼쪽으로 민 것처럼 오른쪽(다음) 탭이 들어온다.
      const forward = this.accumulated > 0;
      this.accumulated = 0;
      this.fired = true;
      this.firedDirection = forward ? 1 : -1;
      this.peakDelta = Math.abs(deltaX);
      this.tailDelta = null;
      this.consumed = true;
      return forward !== this.options.reverse ? "next" : "prev";
    }

    // 발동은 아직이지만 가로로 밀리는 중이면 이벤트를 삼킨다(xterm 가로 스크롤 방지).
    this.consumed = Math.abs(this.accumulated) >= TAB_SWIPE_DEAD_ZONE_PX;
    return null;
  }
}
