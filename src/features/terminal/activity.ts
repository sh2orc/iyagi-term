import { create } from "zustand";

export const ACTIVITY_IDLE_MS = 3000;
export const useTerminalActivity = create<{ sessions: ReadonlySet<string> }>(() => ({ sessions: new Set() }));
const latest = new Map<string, number>();
const timers = new Map<string, ReturnType<typeof setTimeout>>();

/**
 * 세션별 마지막 출력 시각(에이전트 응답 경과 배지용). 화면 글자를 바꾼 live
 * 출력만 기록된다(pipeline `onScreenOutput`, screenOutput.ts) — 이벤트성 제어
 * 바이트로 늘 0초가 되지 않게. 3초 idle 전환으로 지우지 않는다 — "얼마나 오래
 * 조용한가"가 정보이므로 세션이 끝날 때까지 남긴다(clearTerminalActivity에서 정리).
 */
const lastOutputAt = new Map<string, number>();

/** 화면을 바꾼 live 출력이 적용됐다 — 활동 점과 별개로 마지막 출력 시각을 기억한다. */
export function recordSessionOutput(sessionId: string): void {
  lastOutputAt.set(sessionId, Date.now());
}

/** 마지막 live 출력 시각(ms). 아직 출력이 없으면 null. */
export function sessionLastOutputAt(sessionId: string): number | null {
  return lastOutputAt.get(sessionId) ?? null;
}

/** 세션이 끝났다 — 활동 점과 마지막 출력 시각을 모두 지운다. */
export function clearTerminalActivity(sessionId: string): void {
  expireTerminalActivity(sessionId);
  lastOutputAt.delete(sessionId);
}

/**
 * 활동 점(3초 창)만 끈다. 마지막 출력 시각은 건드리지 않는다 — 여기서 함께 지우면
 * 출력이 멈추고 3초 뒤 배지가 사라져 "마지막 출력 N분"이 한 번도 보이지 않는다.
 */
function expireTerminalActivity(sessionId: string): void {
  clearTimeout(timers.get(sessionId));
  timers.delete(sessionId);
  latest.delete(sessionId);
  if (!useTerminalActivity.getState().sessions.has(sessionId)) return;
  useTerminalActivity.setState(s => {
    const sessions = new Set(s.sessions);
    sessions.delete(sessionId);
    return { sessions };
  });
}

/** Only the idle/active transition notifies React, never every output chunk. */
export function markTerminalActivity(sessionId: string): void {
  latest.set(sessionId, Date.now());
  if (timers.has(sessionId)) return;
  const expire = () => {
    const remaining = ACTIVITY_IDLE_MS - (Date.now() - (latest.get(sessionId) ?? 0));
    if (remaining > 0) timers.set(sessionId, setTimeout(expire, remaining));
    else expireTerminalActivity(sessionId);
  };
  timers.set(sessionId, setTimeout(expire, ACTIVITY_IDLE_MS));
  useTerminalActivity.setState(s => ({ sessions: new Set([...s.sessions, sessionId]) }));
}
