/**
 * missionStore selectors(05-ui §5 label 규칙).
 *
 * 모두 원시값(string/number)을 반환한다 — 객체를 새로 만들면 Object.is 비교가
 * 항상 실패해 모든 mission 탭이 매 store 갱신마다 다시 그려진다. 탭 배지는
 * 개수가 바뀔 때만 리렌더되어야 한다(U12와 같은 이유).
 */

import type { RunState } from "../../generated/RunState";
import type { Decision } from "../../generated/Decision";
import type { Finding } from "../../generated/Finding";
import type { Message } from "../../generated/Message";
import type { Mission } from "../../generated/Mission";
import type { Run } from "../../generated/Run";
import type { Task } from "../../generated/Task";
import type { Verification } from "../../generated/Verification";
import type { MissionStoreState } from "./store";

/** live Run(실행 N의 정의, 05 §5): prepared/starting/running/awaiting_input/stopping. */
export const LIVE_RUN_STATES: readonly RunState[] = [
  "prepared",
  "starting",
  "running",
  "awaiting_input",
  "stopping",
];

/** mission 제목 — snapshot을 뽑지 않았거나 사라진 mission이면 null. */
export function selectMissionTitle(state: MissionStoreState, missionId: string): string | null {
  return state.missions[missionId]?.title ?? null;
}

/** plan ordinal 순서의 task 목록(05 §5: 새 task는 ordinal 위치에, 재정렬 금지). */
export function selectTaskList(state: MissionStoreState, missionId: string): Task[] {
  return Object.values(state.tasks)
    .filter((task) => task.mission_id === missionId)
    .sort((a, b) => (a.ordinal !== b.ordinal ? a.ordinal - b.ordinal : a.id < b.id ? -1 : 1));
}

/** `실행 N` — live Run 수(05 §5). */
export function selectLiveRunCount(state: MissionStoreState, missionId: string): number {
  let count = 0;
  for (const run of Object.values(state.runs)) {
    if (run.mission_id === missionId && (LIVE_RUN_STATES as readonly string[]).includes(run.state)) count += 1;
  }
  return count;
}

/** `결정 N` — open Decision 수(05 §5). */
export function selectOpenDecisionCount(state: MissionStoreState, missionId: string): number {
  let count = 0;
  for (const decision of Object.values(state.decisions)) {
    if (decision.mission_id === missionId && decision.state === "open") count += 1;
  }
  return count;
}

/** `완료 N` — succeeded Task 수(05 §5: mission 최종 검증과 구분). */
export function selectSucceededTaskCount(state: MissionStoreState, missionId: string): number {
  let count = 0;
  for (const task of Object.values(state.tasks)) {
    if (task.mission_id === missionId && task.state === "succeeded") count += 1;
  }
  return count;
}

/** 목록 진입용: 저장된 mission 중 최신(생성 역순) 하나. */
export function selectFirstMission(state: MissionStoreState): Mission | null {
  const list = Object.values(state.missions);
  if (list.length === 0) return null;
  return list.reduce((newest, mission) =>
    mission.created_at > newest.created_at ? mission : newest,
  );
}

/** Lead 대화 메시지(생성 순). ISO-8601 UTC는 문자열 비교가 시각 순서와 같다. */
export function selectMissionMessages(state: MissionStoreState, missionId: string): Message[] {
  return Object.values(state.messages)
    .filter((message) => message.mission_id === missionId)
    .sort((a, b) => (a.created_at !== b.created_at ? (a.created_at < b.created_at ? -1 : 1) : a.id < b.id ? -1 : 1));
}

/** task 하나의 실행들(attempt 정렬 — 이력 확장용). */
export function selectTaskRuns(state: MissionStoreState, taskId: string): Run[] {
  return Object.values(state.runs)
    .filter((run) => run.task_id === taskId)
    .sort((a, b) => (a.attempt !== b.attempt ? a.attempt - b.attempt : a.id < b.id ? -1 : 1));
}

/** task의 대표 실행: active_run_id 우선, 없으면 최근 attempt. */
export function selectPrimaryRun(state: MissionStoreState, task: Task): Run | null {
  if (task.active_run_id) {
    const active = state.runs[task.active_run_id];
    if (active) return active;
  }
  const runs = selectTaskRuns(state, task.id);
  return runs.length > 0 ? runs[runs.length - 1] : null;
}

/** open Decision 목록(생성 순) — 배너·패널이 현재 질문을 고를 때 쓴다. */
export function selectOpenDecisions(state: MissionStoreState, missionId: string): Decision[] {
  return Object.values(state.decisions)
    .filter((decision) => decision.mission_id === missionId && decision.state === "open")
    .sort((a, b) => (a.created_at !== b.created_at ? (a.created_at < b.created_at ? -1 : 1) : a.id < b.id ? -1 : 1));
}

/** mission 전체 Decision 목록(생성 순) — obsolete 안내의 링크 대상 포함. */
export function selectMissionDecisions(state: MissionStoreState, missionId: string): Decision[] {
  return Object.values(state.decisions)
    .filter((decision) => decision.mission_id === missionId)
    .sort((a, b) => (a.created_at !== b.created_at ? (a.created_at < b.created_at ? -1 : 1) : a.id < b.id ? -1 : 1));
}

/** candidate에 묶인 검증 목록(시작 순). */
export function selectCandidateVerifications(
  state: MissionStoreState,
  candidateId: string,
): Verification[] {
  return Object.values(state.verifications)
    .filter((verification) => verification.candidate_id === candidateId)
    .sort((a, b) => (a.started_at !== b.started_at ? (a.started_at < b.started_at ? -1 : 1) : a.id < b.id ? -1 : 1));
}

/**
 * candidate에 묶인 검증 목록(데몬 snapshot 순서 = store 삽입 순서). 확정 체크리스트의 무결성 검사가
 * 데몬 accept와 같은 "명령별 첫 통과 검증"을 고를 때 쓴다 — 화면 목록은 selectCandidateVerifications.
 */
export function selectCandidateVerificationsInSnapshotOrder(
  state: MissionStoreState,
  candidateId: string,
): Verification[] {
  return Object.values(state.verifications).filter((verification) => verification.candidate_id === candidateId);
}

/** candidate에 묶인 findings(severity 표시용, 순서는 생성순). */
export function selectCandidateFindings(state: MissionStoreState, candidateId: string): Finding[] {
  return Object.values(state.findings)
    .filter((finding) => finding.candidate_id === candidateId)
    .sort((a, b) => (a.id < b.id ? -1 : 1));
}

/** mission 소속 실행 전체(결과 화면의 실행 이력·사용량 집계용). */
export function selectMissionRuns(state: MissionStoreState, missionId: string): Run[] {
  return Object.values(state.runs)
    .filter((run) => run.mission_id === missionId)
    .sort((a, b) => (a.started_at ?? a.id) < (b.started_at ?? b.id) ? -1 : 1);
}

/**
 * 열린 결정이 영향을 주는 할 일 id — `응답 필요` 필터의 기준(05 §5). 원시값
 * 규칙을 지키려고 정렬된 id를 줄바꿈으로 이은 문자열을 반환한다(빈 문자열 = 없음).
 */
export function selectOpenDecisionTaskKey(state: MissionStoreState, missionId: string): string {
  const ids = new Set<string>();
  for (const decision of Object.values(state.decisions)) {
    if (decision.mission_id !== missionId || decision.state !== "open") continue;
    for (const taskId of decision.affected_task_ids) ids.add(taskId);
  }
  return [...ids].sort().join("\n");
}

/** selectOpenDecisionTaskKey 문자열 → 집합. */
export function taskKeySet(key: string): ReadonlySet<string> {
  return new Set(key.length === 0 ? [] : key.split("\n"));
}

/** live Run 중 가장 먼저 시작한 것의 id(종료 확인 대기 안내의 첫 실행). */
export function selectFirstLiveRunId(state: MissionStoreState, missionId: string): string | null {
  // 시작 전 실행(started_at 없음)은 시작한 실행 뒤로 보낸다.
  const startKey = (run: Run) => run.started_at ?? "￿";
  let first: Run | null = null;
  for (const run of Object.values(state.runs)) {
    if (run.mission_id !== missionId || !(LIVE_RUN_STATES as readonly string[]).includes(run.state)) continue;
    if (
      first === null ||
      startKey(run) < startKey(first) ||
      (startKey(run) === startKey(first) && run.id < first.id)
    ) {
      first = run;
    }
  }
  return first?.id ?? null;
}

/**
 * 이 실행이 아직 할 일을 붙잡고 있는가 — 끝나지 않았고, 종료 미확인(unknown/interrupted)이면
 * 종료 증거(reconciliation_ref)도 없는 경우. RunDetail 제어와 재전송 전제가 같이 쓴다.
 */
export function runHoldsTask(run: Run | null): boolean {
  if (run === null) return false;
  if (run.state === "succeeded" || run.state === "failed" || run.state === "cancelled") return false;
  return !((run.state === "unknown" || run.state === "interrupted") && run.reconciliation_ref);
}

/**
 * 할 일 제어(취소·다시 시도·모델 변경)를 누른 순간의 전제. 재동기화 뒤 자동 재전송은
 * 이 값이 그대로일 때만 보낸다 — 다르면 사용자가 본 화면과 다른 요청이 된다.
 */
export interface TaskActionGuard {
  taskId: string;
  state: Task["state"];
  bindingId: string | null;
  activeRunId: string | null;
  attemptCount: number;
  /** 할 일을 붙잡고 있는 실행 id(없으면 null). */
  holdingRunId: string | null;
}

export function selectTaskActionGuard(state: MissionStoreState, task: Task): TaskActionGuard {
  const primary = selectPrimaryRun(state, task);
  return {
    taskId: task.id,
    state: task.state,
    bindingId: task.binding_id,
    activeRunId: task.active_run_id,
    attemptCount: task.attempt_count,
    holdingRunId: primary !== null && runHoldsTask(primary) ? primary.id : null,
  };
}

/** 최신 store의 할 일이 guard와 같은가(할 일이 사라졌거나 전제가 바뀌었으면 false). */
export function taskActionGuardHolds(state: MissionStoreState, guard: TaskActionGuard): boolean {
  const task = state.tasks[guard.taskId];
  if (!task) return false;
  const now = selectTaskActionGuard(state, task);
  return now.state === guard.state
    && now.bindingId === guard.bindingId
    && now.activeRunId === guard.activeRunId
    && now.attemptCount === guard.attemptCount
    && now.holdingRunId === guard.holdingRunId;
}

/**
 * 선택이 비었을 때 자동으로 고를 할 일(05 §5 빈 상태): 실행 중인 Lead(계획 담당)를
 * 먼저, 없으면 plan 순서상 첫 실행 중 할 일. 실행 중인 것이 없으면 null.
 */
export function selectAutoSelectTaskId(state: MissionStoreState, missionId: string): string | null {
  const running = selectTaskList(state, missionId).filter(
    (task) => task.state === "running" || task.state === "awaiting_input",
  );
  const lead = running.find((task) => task.role === "lead" || task.kind === "plan");
  return (lead ?? running[0])?.id ?? null;
}

/** 이 메시지를 답변 기록으로 남긴 결정(시스템 메시지 → 결정 카드). */
export function selectDecisionByAnswerMessage(
  state: MissionStoreState,
  missionId: string,
  messageId: string,
): Decision | null {
  for (const decision of Object.values(state.decisions)) {
    if (decision.mission_id === missionId && decision.answer_message_id === messageId) return decision;
  }
  return null;
}

/** 사용자 의미의 할 일인가 — 대체된 할 일·검증·통합 단계는 계획 개수에서 뺀다. */
export function isUserFacingTask(task: Task): boolean {
  return task.state !== "superseded" && task.kind !== "verify" && task.kind !== "integrate";
}

/** 계획 카드의 `작업 N개` — isUserFacingTask 기준. */
export function selectUserFacingTaskCount(state: MissionStoreState, missionId: string): number {
  let count = 0;
  for (const task of Object.values(state.tasks)) {
    if (task.mission_id === missionId && isUserFacingTask(task)) count += 1;
  }
  return count;
}
