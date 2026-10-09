/**
 * Pane zoom (04-ui.md §3): Ctrl+= / Ctrl+- 로 포커스된 pane의 글꼴 크기를
 * 조정한다. 확대/축소는 pane 단위(다른 pane은 영향 없음)이며, 크기가
 * 바뀌면 registry.refit() → fit → session.resize 경로로 PTY cols/rows까지
 * 같이 재계산된다.
 *
 * 상한/하한은 split 최소 공간 검사(splitTree.canSplit)가 전제하는 픽셀
 * 공간과의 균형에서 정했다: 6px 미만은 글리프가 렌더링되지 않고, 40px
 * 초과면 최소 분할 pane에서 80cols이 들어가지 않는다.
 */

export const DEFAULT_FONT_SIZE = 13;
export const MIN_FONT_SIZE = 6;
export const MAX_FONT_SIZE = 40;

export type ZoomStep = 1 | -1 | 0;

/** Clamp an arbitrary size into [MIN, MAX]; non-finite input falls back to the default. */
export function clampFontSize(size: number): number {
  if (!Number.isFinite(size)) return DEFAULT_FONT_SIZE;
  return Math.min(MAX_FONT_SIZE, Math.max(MIN_FONT_SIZE, Math.round(size)));
}

/**
 * Next font size for a zoom action. Step 0 resets to the default.
 * Already-clamped inputs stay clamped (하한/상한에서 더 눌러도 값이 같아
 * registry.setFontSize가 no-op이 된다).
 */
export function zoomFontSize(current: number, step: ZoomStep): number {
  if (step === 0) return DEFAULT_FONT_SIZE;
  return clampFontSize(current + step);
}
