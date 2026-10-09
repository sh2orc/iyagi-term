/**
 * Claude Code hooks 연동 상태(프론트 측 얇은 어댑터).
 *
 * 실제 판정 로직은 `hooksIntegration.ts`에 있다 — Claude Code와 Codex가
 * 완전히 같은 셰이프(`{"hooks": {"<Event>": [...]}}`)를 쓰기 때문에
 * 공유한다. 이 모듈은 `cli: "claude"`로 고정해 부르는 타입 별칭일 뿐이다.
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
export type ClaudeHooksStatus = HooksStatus;

export function classifyHooksStatus(
  status: ClaudeHooksStatus | null,
  availability: HooksAvailability,
): HooksView {
  return sharedClassifyHooksStatus(status, availability);
}

/** 설정에 붙여넣을 수동 조각(비-Tauri 폴백 안내용) — Claude의 세 이벤트 전부. */
export function manualSnippet(hookCommand: string): string {
  return sharedManualSnippet("claude", hookCommand);
}

/** diff 대신 보여 줄 "바뀌는 부분" 발췌 — 전체 파일이 아닌 추가 라인들. */
export function addedLines(proposed: string | null): string[] {
  return sharedAddedLines("claude", proposed);
}
