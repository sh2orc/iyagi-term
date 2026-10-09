/**
 * Workload 상태 술어 — store/UI 여러 곳이 같은 "끝난 작업" 정의를 쓴다.
 *
 * 원래 terminalPreferences.ts에 얹혀 있었으나 사용자 설정과 아무 관계가
 * 없어 분리했다(설정 스토어는 preferences.ts 하나가 정본이다).
 */

import type { WorkloadState } from "../generated/WorkloadState";

export const isFinishedWorkload = (state: WorkloadState): boolean =>
  ["SUCCEEDED", "FAILED", "CANCELLED", "INTERRUPTED"].includes(state);
