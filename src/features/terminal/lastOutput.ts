/**
 * 마지막 출력 경과 배지(터미널 상단)의 계산 부분 — 순수 함수와, 표시 갱신용
 * 공유 티커. 컴포넌트는 TerminalPane 안에 있다(LastOutputBadge).
 *
 * 경과 문구는 i18n 키 + 파라미터로 만든다: 언어별 단위 문형이 다르기
 * 때문이다("3분" / "3m"). 세션에서 아직 출력이 없거나 마지막 출력이 10초
 * 이내면 배지가 아예 뜨지 않으므로 "0초" 같은 자리 표현은 두지 않는다.
 */

import { useEffect, useState } from "react";

/**
 * 마지막 출력이 이보다 가까우면 배지를 띄우지 않는다 — 방금 출력한 터미널은
 * 활동 점이 이미 알려 주고, 0~9초처럼 계속 바뀌는 숫자는 눈만 끈다.
 */
export const BADGE_SHOW_AFTER_MS = 10_000;

/** 경과가 배지로 보일 만큼 지났는가(10초 이상). */
export function showsLastOutput(elapsedMs: number): boolean {
  return elapsedMs >= BADGE_SHOW_AFTER_MS;
}

/** 이 시간부터 배지를 눈에 띄게(stale) — 사용자가 오래된 터미널을 골라내는 단서. */
export const STALE_WARN_MS = 5 * 60_000;
/** 이 시간부터 한 단계 더 강조(stale-old) — 사실상 잊힌 터미널. */
export const STALE_OLD_MS = 30 * 60_000;

export interface ElapsedPhrase {
  key: string;
  params: Record<string, number>;
}

/**
 * 경과 ms → 표시 문구의 i18n 키와 파라미터. 계산 단위는 위에서 아래로:
 * 초 → 분 → 시간(분 동반) → 일. 문구가 바뀌는 지점은 초·분·시간 경계라
 * 5초 틱 사이에 같은 문구가 다시 그려지는 일은 드물다.
 */
export function elapsedPhrase(elapsedMs: number): ElapsedPhrase {
  const seconds = Math.floor(Math.max(0, elapsedMs) / 1000);
  if (seconds < 60) return { key: "terminal.pane.lastOutput.seconds", params: { n: seconds } };
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return { key: "terminal.pane.lastOutput.minutes", params: { n: minutes } };
  const hours = Math.floor(minutes / 60);
  const restMinutes = minutes % 60;
  if (hours < 24) {
    return restMinutes === 0
      ? { key: "terminal.pane.lastOutput.hours", params: { n: hours } }
      : { key: "terminal.pane.lastOutput.hoursMinutes", params: { h: hours, m: restMinutes } };
  }
  return { key: "terminal.pane.lastOutput.days", params: { n: Math.floor(hours / 24) } };
}

/** 경과에 따른 강조 단계 — CSS 클래스 뒤에 붙는 이름(빈 문자열이면 평소 표시). */
export function staleClass(elapsedMs: number): string {
  if (elapsedMs >= STALE_OLD_MS) return " stale-old";
  if (elapsedMs >= STALE_WARN_MS) return " stale";
  return "";
}

export const NOW_TICK_MS = 5000;
const nowListeners = new Set<() => void>();
let ticker: ReturnType<typeof setInterval> | null = null;
/** 모듈 하나가 거는 표시 관찰자(첫 구독자가 걸고 마지막 구독자가 뗀다). */
let visibilityWatched = false;

function documentHidden(): boolean {
  return typeof document !== "undefined" && document.hidden;
}

function notifyNow(): void {
  for (const listener of nowListeners) listener();
}

/** 구독자가 있고 창이 보일 때만 돈다 — 숨은 창의 배지를 5초마다 다시 그릴 이유가 없다. */
function startTicker(): void {
  if (ticker !== null || nowListeners.size === 0 || documentHidden()) return;
  ticker = setInterval(notifyNow, NOW_TICK_MS);
}

function stopTicker(): void {
  if (ticker === null) return;
  clearInterval(ticker);
  ticker = null;
}

/** 숨으면 티커를 멈추고, 다시 보이면 곧바로 한 번 맞춘 뒤 다시 돌린다. */
function onVisibilityChange(): void {
  if (documentHidden()) {
    stopTicker();
    return;
  }
  notifyNow();
  startTicker();
}

/**
 * 공유 티커 구독(useNow의 바탕 — 시험도 이 경로를 쓴다). 첫 구독자가 티커와
 * 표시 관찰자를 함께 켜고, 돌려준 해제 함수로 마지막 구독자가 빠지면 둘 다 끈다.
 */
export function subscribeNow(listener: () => void): () => void {
  nowListeners.add(listener);
  if (!visibilityWatched && typeof document !== "undefined") {
    document.addEventListener("visibilitychange", onVisibilityChange);
    visibilityWatched = true;
  }
  startTicker();
  return () => {
    nowListeners.delete(listener);
    if (nowListeners.size > 0) return;
    stopTicker();
    if (visibilityWatched && typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", onVisibilityChange);
    }
    visibilityWatched = false;
  };
}

/**
 * 지금 시각. 경과 배지는 시간 자체에 따라 바뀌므로 주기적 재계산이
 * 필요한데, pane마다 인터벌을 두면 창 수만큼 타이머가 는다 — 하나의 공유
 * 타이머로 묶고, 구독자가 없거나 창이 숨어 있으면 끈다. 창이 다시 보이면
 * 곧바로 맞춘다.
 */
export function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => subscribeNow(() => setNow(Date.now())), []);
  return now;
}
