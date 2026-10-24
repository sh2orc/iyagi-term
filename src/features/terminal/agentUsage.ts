/**
 * Claude 사용량 한도 배지 판정(W: 상태줄 브리지 → `AgentStatus.usage_limits`).
 * 비-React 순수 함수 — 컴포넌트는 결과만 그린다.
 *
 * 판정은 공식 관찰값만 본다(퍼센트 ≥ 100). "언제 막혔는가"는 UI의 문제가
 * 아니고, 리셋 시각도 CLI가 준 값 그대로 쓴다 — 모르면 정적 배지로
 * 퇴화할 뿐 거짓 카운트다운을 만들지 않는다.
 */

import type { SplitNode } from "./splitTree";
import { listLeaves } from "./splitTree";
import type { AgentStatus } from "../../generated/AgentStatus";
import type { AgentUsageLimitWindow } from "../../generated/AgentUsageLimitWindow";

/** 리셋이 지나도 이 유예 동안은 배지를 둔다 — 경계에서 흔들리지 않게. */
export const USAGE_LIMIT_EXPIRY_GRACE_MS = 120_000;

const NO_RESET_SENTINEL = Number.MAX_SAFE_INTEGER;

/** 막힌(≥100%) 창 중 리셋이 가장 이른 것. 동률이면 5시간 창. 없으면 null. */
export function blockedWindow(
  agent: AgentStatus | null | undefined,
): AgentUsageLimitWindow | null {
  const limits = agent?.usage_limits ?? null;
  if (!limits) return null;
  let best: AgentUsageLimitWindow | null = null;
  let bestAt = NO_RESET_SENTINEL;
  for (const window of [limits.five_hour, limits.seven_day]) {
    if (!window || window.used_percent < 100) continue;
    // 리셋을 모르는 창은 정적 배지 폴백 — 시각을 아는 창이 항상 이긴다.
    const at = window.resets_at_unix_ms ?? NO_RESET_SENTINEL;
    if (best === null || at < bestAt) {
      best = window;
      bestAt = at;
    }
  }
  return best;
}

/** 배지를 계속 보일지 — 리셋이 이미 지났어도 유예 동안은 둔다. */
export function usageLimitVisible(
  resetsAtMs: number | null | undefined,
  nowMs: number,
): boolean {
  if (resetsAtMs == null) return true;
  return nowMs <= resetsAtMs + USAGE_LIMIT_EXPIRY_GRACE_MS;
}

/** 탭 배지용 selector 결과(원시값 — missionStatus 인코딩 관례와 같은 이유). */
export type TabUsageLimitResetsAt = number | null | undefined;

/**
 * 탭 전체에서 볼 배지 값: 막힌 pane 중 가장 이른 리셋 ms. 막혔는데 리셋
 * 시각을 모르는 pane만 있으면 null(정적 배지), 막힌 pane이 없으면
 * undefined(배지 없음).
 */
export function tabUsageLimitResetsAt(
  panes: Record<string, { agent?: AgentStatus | null } | undefined>,
  root: SplitNode | null,
): TabUsageLimitResetsAt {
  let best: number | undefined;
  let staticBlocked = false;
  for (const leaf of listLeaves(root)) {
    const window = blockedWindow(panes[leaf.id]?.agent);
    if (!window) continue;
    if (window.resets_at_unix_ms == null) {
      staticBlocked = true;
      continue;
    }
    if (best === undefined || window.resets_at_unix_ms < best) {
      best = window.resets_at_unix_ms;
    }
  }
  if (best !== undefined) return best;
  return staticBlocked ? null : undefined;
}
