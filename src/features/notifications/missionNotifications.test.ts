/**
 * AI 작업 → 알림 센터: 새 결정 필요·확정 대기 진입·실패만, 한 사건 한 번(mission id + 사건 +
 * 결정 id), 앱을 켜기 전부터 있던 상태는 알리지 않고, 보고 있는 작업 탭은 조용히.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Decision } from "../../generated/Decision";
import type { Mission } from "../../generated/Mission";
import { resetMissionStoreForTests, useMissionStore } from "../missions/store";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { missionNotificationId, startMissionNotifications } from "./missionNotifications";
import { useNotificationStore } from "./notificationStore";

const desktop = vi.hoisted(() => ({ calls: [] as Array<[string, string]> }));

vi.mock("./notificationStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./notificationStore")>();
  return {
    ...actual,
    maybeDesktopNotify: (title: string, body: string) => {
      desktop.calls.push([title, body]);
    },
  };
});

const START = Date.parse("2026-09-17T00:00:00.000Z");

function mission(patch: Partial<Mission> = {}): Mission {
  return {
    id: "m-1",
    revision: "1",
    state: "running",
    phase: "implementing",
    title: "로그인 기능",
    open_decision_count: 0,
    candidate_id: null,
    created_at: "2026-09-16T00:00:00.000Z",
    ...patch,
  } as Mission;
}

function decision(id: string, patch: Partial<Decision> = {}): Decision {
  return { id, mission_id: "m-1", state: "open", ...patch } as Decision;
}

function setMission(next: Mission): void {
  useMissionStore.setState((s) => ({ missions: { ...s.missions, [next.id]: next } }));
}

function ids(): string[] {
  return useNotificationStore.getState().items.map((item) => item.id);
}

let stop: (() => void) | null = null;

beforeEach(() => {
  resetMissionStoreForTests();
  useNotificationStore.setState({ items: [], panelOpen: false });
  useWorkbenchStore.setState({ tabs: [], activeTabId: null, page: "terminal" });
  desktop.calls = [];
});

afterEach(() => {
  stop?.();
  stop = null;
  resetMissionStoreForTests();
  useNotificationStore.setState({ items: [], panelOpen: false });
  useWorkbenchStore.setState({ tabs: [], activeTabId: null });
});

describe("mission notifications — 요약 경로(snapshot 없음)", () => {
  it("앱을 켜기 전부터 결정을 기다리던 작업은 알리지 않고, 새로 늘어난 결정만 한 번 알린다", () => {
    setMission(mission({ open_decision_count: 1, revision: "3" }));
    stop = startMissionNotifications(useMissionStore, { now: () => START });
    expect(ids()).toEqual([]);

    setMission(mission({ open_decision_count: 2, revision: "4" }));
    expect(ids()).toEqual([missionNotificationId("m-1", "decision", "r4")]);
    const item = useNotificationStore.getState().items[0];
    expect(item).toMatchObject({ kind: "mission", missionId: "m-1", missionEvent: "decision", acknowledged: false, title: "로그인 기능" });
    expect(desktop.calls).toHaveLength(1);

    // 같은 요약이 다시 와도(재전송·다른 필드 갱신) 중복하지 않는다.
    setMission(mission({ open_decision_count: 2, revision: "4", title: "로그인 기능" }));
    setMission(mission({ open_decision_count: 1, revision: "5" }));
    expect(ids()).toHaveLength(1);
  });

  it("확정 대기 진입과 실패를 알리고, 실패는 데스크톱 알림으로 보내지 않는다", () => {
    setMission(mission());
    stop = startMissionNotifications(useMissionStore, { now: () => START });
    setMission(mission({ phase: "awaiting_acceptance", candidate_id: "cand-1", revision: "2" }));
    expect(ids()).toEqual([missionNotificationId("m-1", "acceptance", "cand-1")]);
    expect(desktop.calls).toHaveLength(1);

    setMission(mission({ phase: "awaiting_acceptance", candidate_id: "cand-1", revision: "3" }));
    expect(ids()).toHaveLength(1);

    setMission(mission({ state: "failed", phase: "done", revision: "4" }));
    expect(ids()[0]).toBe(missionNotificationId("m-1", "failed", null));
    expect(desktop.calls).toHaveLength(1);
  });

  it("앱을 켠 뒤 만들어진 작업은 처음 볼 때 이미 결정을 기다려도 알린다", () => {
    stop = startMissionNotifications(useMissionStore, { now: () => START });
    setMission(mission({ id: "m-new", created_at: "2026-09-17T00:00:05.000Z", open_decision_count: 1, revision: "2" }));
    expect(ids()).toEqual([missionNotificationId("m-new", "decision", "r2")]);
  });
});

describe("mission notifications — snapshot 경로(결정 id)", () => {
  function syncRecord(): void {
    useMissionStore.setState((s) => ({
      sync: { ...s.sync, "m-1": { atSeq: "10", revision: "1", loading: false, error: null, dirty: false, hintSeq: null } },
    }));
  }

  it("처음 알게 된 결정 id는 기준선이고, 새 결정 id만 결정 id를 키로 한 번 알린다", () => {
    setMission(mission());
    stop = startMissionNotifications(useMissionStore, { now: () => START });
    useMissionStore.setState({ decisions: { "d-old": decision("d-old") } });
    syncRecord();
    expect(ids()).toEqual([]);

    useMissionStore.setState((s) => ({ decisions: { ...s.decisions, "d-new": decision("d-new") } }));
    expect(ids()).toEqual([missionNotificationId("m-1", "decision", "d-new")]);

    // snapshot이 다시 적용돼도(같은 결정) 중복 없음 — 요약 수가 늘어도 snapshot 경로가 정본이다.
    useMissionStore.setState((s) => ({ decisions: { ...s.decisions } }));
    setMission(mission({ open_decision_count: 5, revision: "9" }));
    expect(ids()).toHaveLength(1);
  });

  it("사용자가 그 작업 탭을 보고 있으면 남기지 않는다", () => {
    setMission(mission());
    useWorkbenchStore.setState({ tabs: [{ kind: "mission", id: "tab-m", title: "", missionId: "m-1" }], activeTabId: "tab-m" });
    stop = startMissionNotifications(useMissionStore, { now: () => START });
    setMission(mission({ open_decision_count: 1, revision: "2" }));
    expect(ids()).toEqual([]);
  });

  it("해제하면 더 반응하지 않는다", () => {
    setMission(mission());
    stop = startMissionNotifications(useMissionStore, { now: () => START });
    stop();
    stop = null;
    setMission(mission({ state: "failed", revision: "2" }));
    expect(ids()).toEqual([]);
  });
});
