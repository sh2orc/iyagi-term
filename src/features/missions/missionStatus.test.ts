/**
 * 앱 셸의 AI 작업 표시 규칙: 작업 목록 그룹, 탭 배지 우선순위(요약만으로도 판정),
 * revision이 더 새로운 요약 지키기.
 */

import { describe, expect, it } from "vitest";
import type { Mission } from "../../generated/Mission";
import type { MissionStoreState } from "./store";
import {
  decodeMissionTabBadge,
  encodeMissionTabBadge,
  missionListGroup,
  missionTabBadge,
  newerMission,
  repositoryName,
  selectMissionTabBadge,
  type MissionTabBadge,
} from "./missionStatus";

type Summary = Pick<Mission, "state" | "phase" | "open_decision_count" | "archived_at">;
const summary = (patch: Partial<Summary> = {}): Summary => ({
  state: "running",
  phase: "implementing",
  open_decision_count: 0,
  archived_at: null,
  ...patch,
});

describe("missionListGroup", () => {
  it("보관 → 끝남 → 결정 필요 → 확정 대기 → 진행 중 순으로 판정한다", () => {
    expect(missionListGroup(summary({ archived_at: "2026-09-13T00:00:00Z", open_decision_count: 3 }))).toBe("archived");
    expect(missionListGroup(summary({ state: "failed", open_decision_count: 2 }))).toBe("finished");
    expect(missionListGroup(summary({ state: "cancelled" }))).toBe("finished");
    expect(missionListGroup(summary({ state: "completed", phase: "done" }))).toBe("finished");
    expect(missionListGroup(summary({ open_decision_count: 1, phase: "awaiting_acceptance" }))).toBe("decision");
    expect(missionListGroup(summary({ phase: "awaiting_acceptance" }))).toBe("acceptance");
    expect(missionListGroup(summary({ state: "paused" }))).toBe("active");
    expect(missionListGroup(summary({ state: "draft", phase: "planning" }))).toBe("active");
  });
});

describe("missionTabBadge", () => {
  const input = (patch: Partial<Parameters<typeof missionTabBadge>[0]> = {}) => ({
    state: "running" as Mission["state"] | null,
    phase: "implementing" as Mission["phase"] | null,
    decisions: 0,
    liveRuns: null as number | null,
    ...patch,
  });

  it("결정 필요 N > 확정 대기 > 실패 > 실행 중 N > 완료", () => {
    expect(missionTabBadge(input({ decisions: 2, phase: "awaiting_acceptance", liveRuns: 3 }))).toEqual({ kind: "decision", count: 2 });
    expect(missionTabBadge(input({ phase: "awaiting_acceptance", liveRuns: 1 }))).toEqual({ kind: "acceptance" });
    expect(missionTabBadge(input({ state: "failed", phase: "done", decisions: 1 }))).toEqual({ kind: "failed" });
    expect(missionTabBadge(input({ liveRuns: 3 }))).toEqual({ kind: "running", count: 3 });
    expect(missionTabBadge(input({ state: "completed", phase: "done" }))).toEqual({ kind: "done" });
  });

  it("요약만 있는 백그라운드 탭은 실행 수를 모르므로 숫자 없이 실행 중", () => {
    expect(missionTabBadge(input({ liveRuns: null }))).toEqual({ kind: "running", count: null });
    expect(missionTabBadge(input({ liveRuns: 0 }))).toEqual({ kind: "running", count: null });
  });

  it("보일 상태가 없으면 null(일시정지·초안·취소, 아무것도 모름)", () => {
    expect(missionTabBadge(input({ state: "paused" }))).toBeNull();
    expect(missionTabBadge(input({ state: "draft", phase: "planning" }))).toBeNull();
    expect(missionTabBadge(input({ state: "cancelled", phase: "done" }))).toBeNull();
    expect(missionTabBadge(input({ state: null, phase: null }))).toBeNull();
    expect(missionTabBadge(input({ state: null, phase: null, liveRuns: 2 }))).toEqual({ kind: "running", count: 2 });
  });

  it("원시 문자열로 왕복한다(selector가 객체를 새로 만들지 않게)", () => {
    const badges: Array<MissionTabBadge | null> = [
      null,
      { kind: "decision", count: 4 },
      { kind: "acceptance" },
      { kind: "failed" },
      { kind: "running", count: 2 },
      { kind: "running", count: null },
      { kind: "done" },
    ];
    for (const badge of badges) expect(decodeMissionTabBadge(encodeMissionTabBadge(badge))).toEqual(badge);
  });
});

describe("selectMissionTabBadge", () => {
  const mission = (patch: Partial<Mission> = {}): Mission =>
    ({ id: "m-1", revision: "3", state: "running", phase: "implementing", open_decision_count: 0, ...patch }) as Mission;

  function state(patch: Partial<MissionStoreState>): MissionStoreState {
    return { missions: {}, sync: {}, decisions: {}, runs: {}, ...patch } as MissionStoreState;
  }

  it("snapshot을 안 뽑은 탭은 목록 요약의 open_decision_count를 쓴다", () => {
    const s = state({ missions: { "m-1": mission({ open_decision_count: 2 }) } });
    expect(selectMissionTabBadge(s, "m-1")).toEqual({ kind: "decision", count: 2 });
  });

  it("snapshot을 뽑은 탭은 snapshot의 열린 결정·live 실행 수를 쓴다", () => {
    const s = state({
      missions: { "m-1": mission({ open_decision_count: 0 }) },
      sync: { "m-1": { atSeq: "10", revision: "3", loading: false, error: null, dirty: false, hintSeq: null } },
      decisions: {},
      runs: {
        "r-1": { id: "r-1", mission_id: "m-1", state: "running" },
        "r-2": { id: "r-2", mission_id: "m-1", state: "succeeded" },
      } as unknown as MissionStoreState["runs"],
    });
    expect(selectMissionTabBadge(s, "m-1")).toEqual({ kind: "running", count: 1 });
  });

  it("작업을 모르면 배지가 없다", () => {
    expect(selectMissionTabBadge(state({}), "m-9")).toBeNull();
  });
});

describe("repositoryName · newerMission", () => {
  it("경로 끝 조각(끝 구분자·윈도 구분자 포함)", () => {
    expect(repositoryName("/Users/me/work/shop")).toBe("shop");
    expect(repositoryName("/Users/me/work/shop/")).toBe("shop");
    expect(repositoryName("C:\\\\src\\\\app")).toBe("app");
    expect(repositoryName("/")).toBe("/");
  });

  it("revision이 더 새로운 요약만 받아들인다(같으면 들어온 쪽)", () => {
    const current = { id: "m", revision: "7", title: "snapshot" } as Mission;
    expect(newerMission(current, { ...current, revision: "6", title: "old list" }).title).toBe("snapshot");
    expect(newerMission(current, { ...current, revision: "7", title: "same" }).title).toBe("same");
    expect(newerMission(current, { ...current, revision: "8", title: "new" }).title).toBe("new");
    expect(newerMission(undefined, current)).toBe(current);
  });
});
