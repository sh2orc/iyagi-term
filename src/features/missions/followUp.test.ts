/**
 * 끝난 작업에서 새 작업 대화상자 열기(05 §3·§9): 후속 작업 초안은 이전 제목과
 * 원래 목표를 인용하고, 같은 목표로 새 작업은 원래 목표를 그대로 채운다.
 */

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { Mission } from "../../generated/Mission";
import { t, useI18nStore } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { openMissionCreate } from "./entry";
import { newMissionGoalDraft, openNewMissionFrom } from "./followUp";
import { resetMissionClientForTests } from "./clientAccess";

beforeEach(() => {
  useI18nStore.setState({ language: "ko" });
  resetMissionClientForTests();
  useWorkbenchStore.setState({ modal: null, missionCreate: null, tabs: [], activeTabId: null, focusedLeafId: null, panes: {} });
});

afterEach(() => {
  useWorkbenchStore.setState({ modal: null, missionCreate: null });
});

describe("openMissionCreate 목표 초기값", () => {
  it("목표가 있으면 대화상자 상태에 담고, 없거나 공백이면 기존 모양 그대로 연다", () => {
    openMissionCreate("/work/app", { goal: "로그인 오류 수정" });
    expect(useWorkbenchStore.getState().missionCreate).toEqual({ kind: "mission-create", repositoryPath: "/work/app", goal: "로그인 오류 수정" });
    openMissionCreate("/work/app", { goal: "   " });
    expect(useWorkbenchStore.getState().missionCreate).toStrictEqual({ kind: "mission-create", repositoryPath: "/work/app" });
    openMissionCreate("/work/app");
    expect(useWorkbenchStore.getState().missionCreate).toStrictEqual({ kind: "mission-create", repositoryPath: "/work/app" });
  });
});

describe("newMissionGoalDraft", () => {
  it("후속 작업은 이전 제목과 원래 목표를 인용한 초안을 만든다", () => {
    const draft = newMissionGoalDraft({ title: "로그인 기능" }, "follow-up", "  폼 검증을 추가한다\n");
    expect(draft).toBe(t("missions.followUp.goalDraft", { title: "로그인 기능", goal: "폼 검증을 추가한다" }));
    expect(draft).toContain("로그인 기능");
    expect(draft).toContain("폼 검증을 추가한다");
  });

  it("목표 원문을 읽지 못하면 후속 초안은 제목만 인용한다", () => {
    expect(newMissionGoalDraft({ title: "로그인 기능" }, "follow-up", null)).toBe(
      t("missions.followUp.goalDraftNoGoal", { title: "로그인 기능" }),
    );
  });

  it("같은 목표로 새 작업은 원래 목표를 그대로 쓴다", () => {
    expect(newMissionGoalDraft({ title: "로그인 기능" }, "same-goal", "폼 검증을 추가한다")).toBe("폼 검증을 추가한다");
    expect(newMissionGoalDraft({ title: "로그인 기능" }, "same-goal", null)).toBeNull();
  });

  it("작업의 저장소로 대화상자를 연다(client가 없으면 목표 없이)", async () => {
    const mission = { id: "mission-1", title: "로그인 기능", state: "failed", repository_path: "/repo/app", goal_ref: { id: "goal", sha256: "0", bytes: "1", media_type: "text/plain" } } as unknown as Mission;
    await openNewMissionFrom(mission, "same-goal");
    expect(useWorkbenchStore.getState().missionCreate).toStrictEqual({ kind: "mission-create", repositoryPath: "/repo/app" });
  });

  it("후속 작업은 이전 작업 id를 함께 넘겨 이전 결과 위에서 시작하게 한다(계약 E)", async () => {
    const mission = { id: "mission-1", title: "로그인 기능", state: "completed", repository_path: "/repo/app", goal_ref: { id: "goal", sha256: "0", bytes: "1", media_type: "text/plain" } } as unknown as Mission;
    await openNewMissionFrom(mission, "follow-up");
    expect(useWorkbenchStore.getState().missionCreate).toStrictEqual({
      kind: "mission-create",
      repositoryPath: "/repo/app",
      goal: t("missions.followUp.goalDraftNoGoal", { title: "로그인 기능" }),
      followUpOf: "mission-1",
    });
  });
});
