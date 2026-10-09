/**
 * Codex hooks 연동 상태(프론트 측 얇은 어댑터).
 *
 * 실제 판정 로직은 `hooksIntegration.ts`에 있다(Claude Code와 공유) —
 * 관리 이벤트 목록만 다르다: Codex는 `SessionStart`/`SessionEnd`뿐이고
 * Claude의 `Notification`(승인 알림) 같은 훅이 없다. 이 모듈은
 * `cli: "codex"`로 고정해 부르는 타입 별칭일 뿐이다.
 *
 * 동의 계약(§2.1): `proposed`가 있는 동안은 "미리보기"만 보여 주고,
 * 사용자가 적용을 눌러야 `apply`를 부른다. 브라우저 dev(비-Tauri)에서는
 * 수동 안내(붙여넣기 조각)로 폴백한다.
 */

import {
  addedLines as sharedAddedLines,
  classifyHooksStatus as sharedClassifyHooksStatus,
  manualSnippet as sharedManualSnippet,
  type HooksAvailability,
  type HooksStatus,
  type HooksView,
} from "./hooksIntegration";

export type { HooksAvailability, HooksView };
export type CodexHooksStatus = HooksStatus;

export function classifyHooksStatus(
  status: CodexHooksStatus | null,
  availability: HooksAvailability,
): HooksView {
  return sharedClassifyHooksStatus(status, availability);
}

/** 설정에 붙여넣을 수동 조각(비-Tauri 폴백 안내용) — Codex의 두 이벤트 전부. */
export function manualSnippet(hookCommand: string): string {
  return sharedManualSnippet("codex", hookCommand);
}

/** diff 대신 보여 줄 "바뀌는 부분" 발췌 — 전체 파일이 아닌 추가 라인들. */
export function addedLines(proposed: string | null): string[] {
  return sharedAddedLines("codex", proposed);
}
