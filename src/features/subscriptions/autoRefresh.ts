/**
 * 구독 사용량 자동 갱신의 판단 부분(순수 함수).
 *
 * 갱신 한 번이 CLI 두 개(codex·claude)를 띄우고 HTTP를 친다 — 자동 갱신은
 * 드물게, 보는 사람이 있을 때만, 메모리가 넉넉할 때만 한다. 사용자가 직접 누른
 * 새로고침(팝오버 버튼·설정 변경 이벤트)은 이 판단을 거치지 않는다.
 */

import type { PressureLevel } from "../../generated/PressureLevel";

/** 자동 갱신 주기 — 예전 1분은 매번 CLI 두 개를 띄워 메모리를 출렁이게 했다. */
export const SUBSCRIPTION_POLL_MS = 5 * 60_000;
/**
 * 자동 갱신(주기·창 복귀)은 마지막 갱신(수동 포함) 뒤 이만큼은 지나야 한다 —
 * 잠깐 다른 앱을 봤다 돌아온 것으로, 또는 방금 누른 새로고침 직후의 주기
 * 틱으로 CLI를 다시 띄우지 않는다.
 */
export const SUBSCRIPTION_MIN_GAP_MS = 60_000;

export interface AutoRefreshInput {
  now: number;
  /** 마지막 갱신(수동·자동) 시작 시각. 아직 한 번도 없으면 null. */
  lastRefreshAt: number | null;
  /** 창이 숨어 있는가(트레이·최소화·다른 앱). */
  hidden: boolean;
  /** 호스트 메모리 압력. 아직 모르면(첫 샘플 전) null — 막지 않는다. */
  pressure: PressureLevel | null;
  /** 마지막 갱신 뒤 지나야 하는 시간(0이면 간격을 보지 않는다). */
  minGapMs: number;
}

/**
 * 자동 갱신을 지금 해도 되는가: 보이는 창이고, 메모리 압력이 NORMAL(또는
 * 모름)이며, 마지막 갱신 뒤 `minGapMs`가 지났을 때만. 압력이 높을 때 CLI를
 * 띄우면 그 기동 비용이 그대로 압력에 얹힌다 — 다음 주기 틱이 다시 본다.
 */
export function autoRefreshAllowed(input: AutoRefreshInput): boolean {
  if (input.hidden) return false;
  if (input.pressure !== null && input.pressure !== "NORMAL") return false;
  if (input.lastRefreshAt !== null && input.now - input.lastRefreshAt < input.minGapMs) return false;
  return true;
}
