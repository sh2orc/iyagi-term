/**
 * 탭 닫기 규칙: mission 탭은 로컬 숨김 + "계속 실행 · 목록에서 다시 열기" 안내, terminal 탭은 pane 닫기 확인.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Mission } from "../generated/Mission";
import { t } from "../i18n";
import { resetMissionStoreForTests, useMissionStore } from "../features/missions/store";
import { makeLeaf } from "../features/terminal/splitTree";
import { useWorkbenchStore } from "../store/workbenchStore";
import { missionTabClosedNoticeKey, requestCloseTab } from "./tabCommands";

beforeEach(() => {
  vi.useFakeTimers();
  resetMissionStoreForTests();
  useWorkbenchStore.setState({
    tabs: [
      { kind: "terminal", id: "tab-t", title: "zsh", root: makeLeaf("leaf-1", "view-1") },
      { kind: "mission", id: "tab-m", title: "로그인", missionId: "m-1" },
    ],
    activeTabId: "tab-m",
    focusedLeafId: null,
    toast: null,
    hiddenMissions: new Set<string>(),
  });
});

afterEach(() => {
  vi.useRealTimers();
  resetMissionStoreForTests();
  useWorkbenchStore.setState({ tabs: [], activeTabId: null, toast: null, hiddenMissions: new Set<string>() });
});

describe("requestCloseTab", () => {
  it("실행 중인 작업 탭을 닫으면 숨기고, 계속 실행되며 목록에서 다시 열 수 있다고 잠깐 알린다", () => {
    useMissionStore.setState({ missions: { "m-1": { id: "m-1", state: "running" } as Mission } });
    const controller = { requestClosePanes: vi.fn() };
    requestCloseTab(controller, "tab-m");
    const state = useWorkbenchStore.getState();
    expect(state.tabs.map((tab) => tab.id)).toEqual(["tab-t"]);
    expect(state.hiddenMissions.has("m-1")).toBe(true);
    expect(state.toast).toBe(t("missions.tabClosed.running"));
    expect(controller.requestClosePanes).not.toHaveBeenCalled();
    vi.advanceTimersByTime(10_000);
    expect(useWorkbenchStore.getState().toast).toBeNull();
  });

  it("끝난 작업에는 '계속 실행됩니다'라고 하지 않는다", () => {
    expect(missionTabClosedNoticeKey("completed")).toBe("missions.tabClosed.finished");
    expect(missionTabClosedNoticeKey("failed")).toBe("missions.tabClosed.finished");
    expect(missionTabClosedNoticeKey("running")).toBe("missions.tabClosed.running");
    // 요약이 아직 없으면(모름) 실행 중으로 안내한다 — 닫는다고 취소되지 않는다는 사실이 더 중요하다.
    expect(missionTabClosedNoticeKey(null)).toBe("missions.tabClosed.running");
  });

  it("다른 안내가 그사이 떴으면 시간이 지나도 지우지 않는다", () => {
    requestCloseTab({ requestClosePanes: vi.fn() }, "tab-m");
    useWorkbenchStore.getState().setToast("더 중요한 안내");
    vi.advanceTimersByTime(10_000);
    expect(useWorkbenchStore.getState().toast).toBe("더 중요한 안내");
  });

  it("terminal 탭은 pane 닫기 확인으로 보내고 안내 토스트를 띄우지 않는다", () => {
    const controller = { requestClosePanes: vi.fn() };
    requestCloseTab(controller, "tab-t");
    expect(controller.requestClosePanes).toHaveBeenCalledWith(["leaf-1"], "tab-t");
    expect(useWorkbenchStore.getState().toast).toBeNull();
  });
});
