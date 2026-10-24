/**
 * autoContinue 섹션 — 네트워크 끊김 자동 continue(감지·회복 전송·해제)의
 * 토스트·알림 문구. reason 키는 데몬의 disarmed reason 문자열과 1:1.
 */
import type { SectionMessages } from "../core";

export const ko = {
  "autoContinue.armed": "네트워크 끊김 감지 — 회복되면 자동으로 continue를 보냅니다",
  "autoContinue.fired": "네트워크 회복 — continue를 보냈습니다",
  "autoContinue.disarmed": "자동 계속을 걷었습니다: {reason}",

  "autoContinue.reason.session-gone": "터미널이 종료됨",
  "autoContinue.reason.guard-suspended": "자원 가드로 일시정지 중",
  "autoContinue.reason.user-input": "직접 입력이 감지됨",
  "autoContinue.reason.write-failed": "전송 실패",
  "autoContinue.reason.unknown": "기타",
};

export const en = {
  "autoContinue.armed": "Network is down — will send continue automatically once it recovers",
  "autoContinue.fired": "Network recovered — sent continue",
  "autoContinue.disarmed": "Auto-continue stopped: {reason}",

  "autoContinue.reason.session-gone": "the terminal ended",
  "autoContinue.reason.guard-suspended": "suspended by the resource guard",
  "autoContinue.reason.user-input": "manual input detected",
  "autoContinue.reason.write-failed": "write failed",
  "autoContinue.reason.unknown": "other",
};

export const autoContinueSection: SectionMessages = { ko, en };
