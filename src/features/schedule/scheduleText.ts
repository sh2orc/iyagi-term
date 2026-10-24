/**
 * 예약 명령 표시 문자열 유틸(비-React — 호출 시점 t()).
 * 상태·이유 키는 데몬의 ScheduledCommandStatus/reason_code와 1:1이다.
 */
import type { ScheduledCommandStatus } from "../../generated/ScheduledCommandStatus";
import type { MessageParams } from "../../i18n/core";

/** 코어 t()/useI18n().t와 같은 계약(테스트에서도 문구 주입이 가능하다). */
export type TFunc = (key: string, params?: MessageParams) => string;

/** 상태 배지 문구 키 — 알 수 없는 상태는 pending으로 표시하지 않고 원문을 믿는다. */
export function scheduleStatusKey(status: ScheduledCommandStatus): string {
  return `schedule.status.${status}`;
}

/** 이유 문구. 모르는 코드는 원문 그대로 보여 준다(데몬이 새 코드를 보내도 거짓말하지 않는다). */
export function scheduleReasonText(
  reason: string | null | undefined,
  t: TFunc,
): string | null {
  if (reason == null || reason === "") return null;
  switch (reason) {
    case "session-gone":
    case "guard-suspended":
    case "session-exited":
    case "write-failed":
    case "daemon-restart":
      return t(`schedule.reason.${reason}`);
    default:
      return reason;
  }
}

/** 남은 시간을 "2시간 3분 20초"·"3분"·"45초" 형태로. 0인 뒤쪽 단위는 생략. */
export function formatDuration(ms: number, t: TFunc): string {
  const totalSeconds = Math.max(0, Math.ceil(ms / 1000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const parts: string[] = [];
  if (hours > 0) parts.push(t("schedule.duration.h", { n: hours }));
  if (minutes > 0) parts.push(t("schedule.duration.m", { n: minutes }));
  // 값이 0이면 가장 작은 단위 하나만 남긴다("0초").
  if (seconds > 0 || parts.length === 0) parts.push(t("schedule.duration.s", { n: seconds }));
  return parts.join(" ");
}

/** 카운트다운 문구 — 1초 미만이면 "곧 실행". */
export function countdownText(fireAtMs: number, nowMs: number, t: TFunc): string {
  const remaining = fireAtMs - nowMs;
  if (remaining <= 0) return t("schedule.countdown.now");
  return t("schedule.countdown", { time: formatDuration(remaining, t) });
}

/** `datetime-local` 입력값(로컬 시각 "YYYY-MM-DDTHH:mm")으로 변환. */
export function toLocalInputValue(ms: number): string {
  const date = new Date(ms);
  const pad = (n: number): string => `${n}`.padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** `datetime-local` 입력값(로컬 시각)을 epoch ms로. 못 쓴 값은 NaN. */
export function fromLocalInputValue(value: string): number {
  return new Date(value).getTime();
}

/** UTF-8 바이트 길이(명령 상한 검증용 — 데몬 규칙과 같은 단위). */
export function utf8ByteLength(text: string): number {
  return new TextEncoder().encode(text).length;
}

/**
 * 명령 원문 상한(term-contracts schedule::limits::COMMAND_MAX_BYTES의
 * 거울 — ts-rs는 상수를 내보내지 않으므로 여기서 유지한다).
 */
export const COMMAND_MAX_BYTES = 4_000;
