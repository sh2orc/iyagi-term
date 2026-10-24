/**
 * schedule 섹션 — 예약 명령 다이얼로그·상태 배지·토스트/알림 문구.
 * 상태(`schedule.status.*`)와 이유(`schedule.reason.*`)는 데몬의
 * ScheduledCommandStatus/reason_code 문자열과 1:1이다.
 */
import type { SectionMessages } from "../core";

export const ko = {
  "schedule.title": "명령 예약",
  "schedule.hint": "지정한 시각에 선택한 터미널로 명령이 전송됩니다. 창을 닫아도 데몬이 실행합니다.",
  "schedule.target": "대상 터미널",
  "schedule.target.none": "열린 터미널이 없습니다",
  "schedule.command": "명령",
  "schedule.command.placeholder": "예: cargo test",
  "schedule.command.hint": "명령 끝에 Enter가 함께 전송됩니다.",
  "schedule.time": "실행 시각",
  "schedule.quick.1m": "+1분",
  "schedule.quick.5m": "+5분",
  "schedule.quick.30m": "+30분",
  "schedule.quick.1h": "+1시간",
  "schedule.submit": "예약",
  "schedule.close": "닫기",

  "schedule.list.title": "예약 목록",
  "schedule.list.pending": "대기 중",
  "schedule.list.history": "최근 기록",
  "schedule.list.empty": "예약이 없습니다.",
  "schedule.list.cancel": "취소",
  "schedule.countdown": "{time} 후",
  "schedule.countdown.now": "곧 실행",
  "schedule.duration.h": "{n}시간",
  "schedule.duration.m": "{n}분",
  "schedule.duration.s": "{n}초",

  "schedule.status.pending": "대기 중",
  "schedule.status.firing": "전송 중",
  "schedule.status.fired": "실행됨",
  "schedule.status.failed": "실패",
  "schedule.status.skipped": "건너뜀",
  "schedule.status.cancelled": "취소됨",

  "schedule.reason.session-gone": "대상 터미널이 이미 끝났습니다",
  "schedule.reason.guard-suspended": "자원 가드로 일시정지 중인 터미널입니다",
  "schedule.reason.session-exited": "터미널이 종료되어 건너뛰었습니다",
  "schedule.reason.write-failed": "터미널에 쓰지 못했습니다",
  "schedule.reason.daemon-restart": "실행 관리자가 재시작되어 건너뛰었습니다",
  "schedule.reason.unknown": "알 수 없는 사유",

  "schedule.error.past": "실행 시각은 현재보다 미래여야 합니다.",
  "schedule.error.command": "명령은 1..=4000바이트여야 합니다.",
  "schedule.error.create": "예약하지 못했습니다: {message}",

  "schedule.toast.fired": "예약 명령 실행됨: {command}",
  "schedule.toast.failed": "예약 명령 실패: {command}",
  "schedule.toast.skipped": "예약 명령 건너뜀: {command} — {reason}",
};

export const en = {
  "schedule.title": "Schedule a command",
  "schedule.hint": "The command is sent to the selected terminal at the chosen time. The daemon fires it even with the window closed.",
  "schedule.target": "Target terminal",
  "schedule.target.none": "No open terminals",
  "schedule.command": "Command",
  "schedule.command.placeholder": "e.g. cargo test",
  "schedule.command.hint": "An Enter is sent after the command.",
  "schedule.time": "Run at",
  "schedule.quick.1m": "+1m",
  "schedule.quick.5m": "+5m",
  "schedule.quick.30m": "+30m",
  "schedule.quick.1h": "+1h",
  "schedule.submit": "Schedule",
  "schedule.close": "Close",

  "schedule.list.title": "Scheduled commands",
  "schedule.list.pending": "Pending",
  "schedule.list.history": "Recent",
  "schedule.list.empty": "No scheduled commands.",
  "schedule.list.cancel": "Cancel",
  "schedule.countdown": "in {time}",
  "schedule.countdown.now": "any moment",
  "schedule.duration.h": "{n}h",
  "schedule.duration.m": "{n}m",
  "schedule.duration.s": "{n}s",

  "schedule.status.pending": "Pending",
  "schedule.status.firing": "Sending",
  "schedule.status.fired": "Ran",
  "schedule.status.failed": "Failed",
  "schedule.status.skipped": "Skipped",
  "schedule.status.cancelled": "Cancelled",

  "schedule.reason.session-gone": "the target terminal already ended",
  "schedule.reason.guard-suspended": "the terminal is suspended by the resource guard",
  "schedule.reason.session-exited": "the terminal exited before the scheduled time",
  "schedule.reason.write-failed": "could not write to the terminal",
  "schedule.reason.daemon-restart": "the daemon restarted before the scheduled time",
  "schedule.reason.unknown": "unknown reason",

  "schedule.error.past": "The run time must be in the future.",
  "schedule.error.command": "The command must be 1..=4000 bytes.",
  "schedule.error.create": "Could not schedule: {message}",

  "schedule.toast.fired": "Scheduled command ran: {command}",
  "schedule.toast.failed": "Scheduled command failed: {command}",
  "schedule.toast.skipped": "Scheduled command skipped: {command} — {reason}",
};

export const scheduleSection: SectionMessages = { ko, en };
