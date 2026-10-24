/**
 * 예약 명령 표시 유틸: 기간·카운트다운 문구, datetime-local 왕복,
 * 상태·이유 키 매핑, 바이트 상한.
 */
import { describe, expect, it } from "vitest";
import { t } from "../../i18n";
import {
  COMMAND_MAX_BYTES,
  countdownText,
  formatDuration,
  fromLocalInputValue,
  scheduleReasonText,
  scheduleStatusKey,
  toLocalInputValue,
  utf8ByteLength,
} from "./scheduleText";

describe("formatDuration/countdownText", () => {
  it("시·분·초를 i18n 단위로 조립한다", () => {
    expect(formatDuration(0, t)).toBe(t("schedule.duration.s", { n: 0 }));
    expect(formatDuration(45_000, t)).toBe(t("schedule.duration.s", { n: 45 }));
    expect(formatDuration(3 * 60_000, t)).toBe(t("schedule.duration.m", { n: 3 }));
    // 2시간 3분 20초처럼 큰 값도 내려가는 단위는 생략한다.
    expect(formatDuration((2 * 3600 + 3 * 60 + 20) * 1000, t)).toBe(
      [
        t("schedule.duration.h", { n: 2 }),
        t("schedule.duration.m", { n: 3 }),
        t("schedule.duration.s", { n: 20 }),
      ].join(" "),
    );
  });

  it("카운트다운은 남은 시간을 붙이고 지나면 '곧 실행'이다", () => {
    const now = 1_000_000_000;
    expect(countdownText(now + 30_000, now, t)).toBe(
      t("schedule.countdown", { time: t("schedule.duration.s", { n: 30 }) }),
    );
    expect(countdownText(now, now, t)).toBe(t("schedule.countdown.now"));
    expect(countdownText(now - 5_000, now, t)).toBe(t("schedule.countdown.now"));
  });
});

describe("datetime-local 왕복", () => {
  it("로컬 시각 문자열과 epoch ms가 서로 되돌아간다(분 단위)", () => {
    const ms = new Date(2026, 9, 20, 14, 5).getTime();
    expect(toLocalInputValue(ms)).toBe("2026-10-20T14:05");
    expect(fromLocalInputValue(toLocalInputValue(ms))).toBe(ms);
  });

  it("못 쓴 값은 NaN", () => {
    expect(Number.isNaN(fromLocalInputValue(""))).toBe(true);
    expect(Number.isNaN(fromLocalInputValue("not-a-date"))).toBe(true);
  });
});

describe("상태·이유 문구", () => {
  it("상태 키는 그대로 접두를 붙인다", () => {
    expect(scheduleStatusKey("pending")).toBe("schedule.status.pending");
    expect(scheduleStatusKey("fired")).toBe("schedule.status.fired");
  });

  it("아는 이유는 문구로, 모르는 이유는 원문으로", () => {
    expect(scheduleReasonText("session-gone", t)).toBe(t("schedule.reason.session-gone"));
    expect(scheduleReasonText("guard-suspended", t)).toBe(t("schedule.reason.guard-suspended"));
    expect(scheduleReasonText("brand-new-code", t)).toBe("brand-new-code");
    expect(scheduleReasonText(null, t)).toBeNull();
    expect(scheduleReasonText("", t)).toBeNull();
  });
});

describe("명령 바이트 상한", () => {
  it("한글은 바이트로 센다(데몬 규칙과 같은 단위)", () => {
    expect(utf8ByteLength("cargo test")).toBe(10);
    // 한글 낱글 3바이트 × 3.
    expect(utf8ByteLength("명령을")).toBe(9);
    expect(utf8ByteLength("x".repeat(COMMAND_MAX_BYTES))).toBe(COMMAND_MAX_BYTES);
  });
});
