/**
 * missionStore selectors(05-ui §5 label 규칙): 실행 N = live Run
 * (prepared/starting/running/awaiting_input/stopping), 완료 N = succeeded
 * Task, 결정 N = open Decision. 모두 원시값을 반환해 개수가 바뀔 때만
 * 탭 배지가 다시 그려진다.
 */

import { beforeEach, describe, expect, it } from "vitest";
import type { Decision } from "../../generated/Decision";
import type { Mission } from "../../generated/Mission";
import type { Run } from "../../generated/Run";
import type { Task } from "../../generated/Task";
import { resetMissionStoreForTests, useMissionStore } from "./store";
import {
  LIVE_RUN_STATES,
  isUserFacingTask,
  selectAutoSelectTaskId,
  selectDecisionByAnswerMessage,
  selectFirstLiveRunId,
  selectOpenDecisionTaskKey,
  selectUserFacingTaskCount,
  taskKeySet,
  selectFirstMission,
  selectLiveRunCount,
  selectMissionTitle,
  selectOpenDecisionCount,
  selectSucceededTaskCount,
  selectTaskList,
} from "./selectors";

const mission = (id: string, title: string, createdAt: string): Mission =>
  ({ id, revision: "1", state: "running", title, created_at: createdAt }) as unknown as Mission;
const task = (id: string, missionId: string, ordinal: number, state: Task["state"]): Task =>
  ({ id, mission_id: missionId, ordinal, state, title: `task-${id}` }) as unknown as Task;
const run = (id: string, missionId: string, state: Run["state"]): Run =>
  ({ id, mission_id: missionId, task_id: "t-1", state }) as unknown as Run;
const decision = (id: string, missionId: string, state: Decision["state"]): Decision =>
  ({ id, mission_id: missionId, state }) as unknown as Decision;

beforeEach(() => {
  resetMissionStoreForTests();
  useMissionStore.setState({
    missions: {
      "m-1": mission("m-1", "로그인 기능", "2026-01-02T00:00:00Z"),
      "m-2": mission("m-2", "성능 개선", "2026-01-01T00:00:00Z"),
    },
    tasks: {
      "t-3": task("t-3", "m-1", 3, "running"),
      "t-1": task("t-1", "m-1", 1, "succeeded"),
      "t-2": task("t-2", "m-1", 2, "planned"),
      "t-x": task("t-x", "m-2", 1, "succeeded"),
    },
    runs: {
      r1: run("r1", "m-1", "prepared"),
      r2: run("r2", "m-1", "starting"),
      r3: run("r3", "m-1", "running"),
      r4: run("r4", "m-1", "awaiting_input"),
      r5: run("r5", "m-1", "stopping"),
      r6: run("r6", "m-1", "succeeded"),
      r7: run("r7", "m-1", "failed"),
      r8: run("r8", "m-1", "cancelled"),
      r9: run("r9", "m-1", "interrupted"),
      r10: run("r10", "m-1", "unknown"),
      rOther: run("rOther", "m-2", "running"),
    },
    decisions: {
      d1: decision("d1", "m-1", "open"),
      d2: decision("d2", "m-1", "answered"),
      d3: decision("d3", "m-1", "obsolete"),
      dOther: decision("dOther", "m-2", "open"),
    },
  });
});

describe("mission selectors — 05 §5 label 규칙", () => {
  it("selectTaskList는 plan ordinal 순서로 반환한다(재정렬 금지)", () => {
    const state = useMissionStore.getState();
    expect(selectTaskList(state, "m-1").map(task => task.id)).toEqual(["t-1", "t-2", "t-3"]);
    expect(selectTaskList(state, "m-2").map(task => task.id)).toEqual(["t-x"]);
    expect(selectTaskList(state, "nope")).toEqual([]);
  });

  it("selectLiveRunCount: 실행 N = live Run(prepared/starting/running/awaiting_input/stopping)", () => {
    const state = useMissionStore.getState();
    expect(LIVE_RUN_STATES).toEqual(["prepared", "starting", "running", "awaiting_input", "stopping"]);
    expect(selectLiveRunCount(state, "m-1")).toBe(5); // r1..r5
    expect(selectLiveRunCount(state, "m-2")).toBe(1); // 다른 mission의 live run은 세지 않는다
    expect(selectLiveRunCount(state, "nope")).toBe(0);
  });

  it("selectOpenDecisionCount: 결정 N = open Decision 수", () => {
    const state = useMissionStore.getState();
    expect(selectOpenDecisionCount(state, "m-1")).toBe(1); // answered/obsolete 제외
    expect(selectOpenDecisionCount(state, "m-2")).toBe(1);
    expect(selectOpenDecisionCount(state, "nope")).toBe(0);
  });

  it("selectSucceededTaskCount: 완료 N = succeeded Task 수(mission 최종 검증과 구분)", () => {
    const state = useMissionStore.getState();
    expect(selectSucceededTaskCount(state, "m-1")).toBe(1);
    expect(selectSucceededTaskCount(state, "m-2")).toBe(1);
    expect(selectSucceededTaskCount(state, "nope")).toBe(0);
  });

  it("selectMissionTitle: snapshot을 뽑지 않았거나 사라진 mission이면 null", () => {
    const state = useMissionStore.getState();
    expect(selectMissionTitle(state, "m-1")).toBe("로그인 기능");
    expect(selectMissionTitle(state, "nope")).toBeNull();
  });

  it("selectFirstMission: 저장된 mission 중 최신 생성 하나", () => {
    expect(selectFirstMission(useMissionStore.getState())?.id).toBe("m-1");
    resetMissionStoreForTests();
    expect(selectFirstMission(useMissionStore.getState())).toBeNull();
  });
});

describe("mission selectors — 실행 중 작업 화면 보강", () => {
  it("selectOpenDecisionTaskKey: 열린 결정의 영향 할 일만 원시 문자열로 모은다", () => {
    useMissionStore.setState((state) => ({
      decisions: {
        ...state.decisions,
        d1: { ...state.decisions.d1, affected_task_ids: ["t-3", "t-1"] },
        d2: { ...state.decisions.d2, affected_task_ids: ["t-2"] },
        d4: { ...decision("d4", "m-1", "open"), affected_task_ids: ["t-1"] },
      },
    }));
    const state = useMissionStore.getState();
    const key = selectOpenDecisionTaskKey(state, "m-1");
    expect(key).toBe(selectOpenDecisionTaskKey(state, "m-1"));
    expect([...taskKeySet(key)]).toEqual(["t-1", "t-3"]);
    expect(taskKeySet(selectOpenDecisionTaskKey(state, "nope")).size).toBe(0);
  });

  it("selectFirstLiveRunId: 가장 먼저 시작한 live Run, 시작 전 실행은 뒤로", () => {
    useMissionStore.setState({
      runs: {
        late: { ...run("late", "m-1", "running"), started_at: "2026-01-01T00:05:00Z" },
        early: { ...run("early", "m-1", "stopping"), started_at: "2026-01-01T00:01:00Z" },
        pending: { ...run("pending", "m-1", "prepared"), started_at: null },
        done: { ...run("done", "m-1", "succeeded"), started_at: "2025-12-31T00:00:00Z" },
      },
    });
    expect(selectFirstLiveRunId(useMissionStore.getState(), "m-1")).toBe("early");
    expect(selectFirstLiveRunId(useMissionStore.getState(), "m-2")).toBeNull();
  });

  it("selectAutoSelectTaskId: 실행 중인 Lead 우선, 없으면 plan 순서상 첫 실행 중 할 일", () => {
    const state = useMissionStore.getState();
    expect(selectAutoSelectTaskId(state, "m-1")).toBe("t-3");
    useMissionStore.setState((current) => ({
      tasks: {
        ...current.tasks,
        lead: { ...task("lead", "m-1", 9, "running"), kind: "plan", role: "lead" } as Task,
      },
    }));
    expect(selectAutoSelectTaskId(useMissionStore.getState(), "m-1")).toBe("lead");
    expect(selectAutoSelectTaskId(useMissionStore.getState(), "m-2")).toBeNull();
  });

  it("selectDecisionByAnswerMessage: 답변 기록 메시지로 결정을 찾는다", () => {
    useMissionStore.setState((state) => ({
      decisions: { ...state.decisions, d2: { ...state.decisions.d2, answer_message_id: "msg-9" } },
    }));
    expect(selectDecisionByAnswerMessage(useMissionStore.getState(), "m-1", "msg-9")?.id).toBe("d2");
    expect(selectDecisionByAnswerMessage(useMissionStore.getState(), "m-2", "msg-9")).toBeNull();
  });

  it("isUserFacingTask: 대체됨·검증·통합은 계획 개수에서 뺀다", () => {
    const base = task("u", "m-1", 1, "planned");
    expect(isUserFacingTask({ ...base, kind: "implement" } as Task)).toBe(true);
    expect(isUserFacingTask({ ...base, kind: "verify" } as Task)).toBe(false);
    expect(isUserFacingTask({ ...base, kind: "integrate" } as Task)).toBe(false);
    expect(isUserFacingTask({ ...base, kind: "implement", state: "superseded" } as Task)).toBe(false);
    useMissionStore.setState({
      tasks: {
        a: { ...task("a", "m-1", 1, "running"), kind: "implement" } as Task,
        b: { ...task("b", "m-1", 2, "planned"), kind: "verify" } as Task,
        c: { ...task("c", "m-1", 3, "superseded"), kind: "design" } as Task,
      },
    });
    expect(selectUserFacingTaskCount(useMissionStore.getState(), "m-1")).toBe(1);
  });
});

