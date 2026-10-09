/**
 * agent-view 탭(05 §2·§6): 이 보기가 요청한 동기화가 끝나기 전에는 "기록 없음"을 판단하지 않고
 * 로딩으로 보이며, 끝난 뒤에도 할 일이 없을 때만 탭 닫기를 권한다.
 */

import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { t } from "../../i18n";
import { MissionAgentView } from "./MissionAgentView";
import { useMissionStore } from "./store";
import { fakeMission, fakeTask, flushAsync, renderUi, resetAllMissionState, seedStore } from "./testSupport";

vi.mock("./RunDetail", async () => {
  const { createElement } = await import("react");
  return {
    RunDetail: (props: { task: { id: string; title: string } }) =>
      createElement("div", { "data-testid": "run-detail", "data-task-id": props.task.id }, props.task.title),
  };
});

const realSyncMission = useMissionStore.getState().syncMission;

beforeEach(() => {
  resetAllMissionState();
  useMissionStore.setState({ syncMission: realSyncMission });
});

function settledStatus(missionId: string): void {
  useMissionStore.setState((state) => ({
    sync: { ...state.sync, [missionId]: { atSeq: "5", revision: "5", loading: false, error: null, dirty: false, hintSeq: null } },
  }));
}

describe("MissionAgentView 기록 없음 판단", () => {
  it("이전 동기화 결과에 할 일이 없어도, 이 보기의 동기화가 끝나기 전에는 기록 없음 대신 로딩을 보인다", async () => {
    const mission = fakeMission();
    const task = fakeTask(mission.id, "running");
    seedStore({ missions: [mission] });
    settledStatus(mission.id);
    const pending: { finish: (() => void) | null } = { finish: null };
    useMissionStore.setState({
      syncMission: (missionId: string) =>
        new Promise<void>((resolve) => {
          pending.finish = () => {
            seedStore({ tasks: [task] });
            settledStatus(missionId);
            resolve();
          };
        }),
    });
    const ui = renderUi(<MissionAgentView missionId={mission.id} taskId={task.id} />);
    try {
      await flushAsync(2);
      expect(ui.container.querySelector("[data-testid=mission-not-found]")).toBeNull();
      expect(ui.container.textContent).toContain(t("missions.sync.loading"));
      act(() => pending.finish?.());
      await flushAsync(2);
      expect(ui.container.querySelector("[data-testid=mission-not-found]")).toBeNull();
      expect(ui.container.querySelector("[data-testid=run-detail]")?.getAttribute("data-task-id")).toBe(task.id);
    } finally {
      ui.unmount();
      useMissionStore.setState({ syncMission: realSyncMission });
    }
  });

  it("이 보기의 동기화가 끝났는데도 할 일이 없으면 탭 닫기를 권한다", async () => {
    const mission = fakeMission();
    seedStore({ missions: [mission] });
    useMissionStore.setState({
      syncMission: async (missionId: string) => {
        settledStatus(missionId);
      },
    });
    const ui = renderUi(<MissionAgentView missionId={mission.id} taskId="task-gone" />);
    try {
      await flushAsync(2);
      expect(ui.container.querySelector("[data-testid=mission-not-found]")?.textContent).toContain(t("missions.notFound.title"));
    } finally {
      ui.unmount();
      useMissionStore.setState({ syncMission: realSyncMission });
    }
  });
});
