/**
 * AI 작업 목록(modal kind "mission-list"): 그룹·검색·탭 열기(이미 열린 탭은 이동)·
 * 탭 상한 안내(대화상자 안)·빈 상태·보관 해제·작업 공간 사용량과 정리(계약 D).
 */

import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RpcClientError, type DaemonClient } from "../daemon/client";
import type { WorkspaceCleanupParams } from "../../generated/WorkspaceCleanupParams";
import type { WorkspaceCleanupResult } from "../../generated/WorkspaceCleanupResult";
import type { WorkspaceUsageParams } from "../../generated/WorkspaceUsageParams";
import type { WorkspaceUsageResult } from "../../generated/WorkspaceUsageResult";
import type { Mission } from "../../generated/Mission";
import type { MissionControlParams } from "../../generated/MissionControlParams";
import type { MissionListParams } from "../../generated/MissionListParams";
import type { MutationResult } from "../../generated/MutationResult";
import { t, useI18nStore } from "../../i18n";
import { MAX_MISSION_TABS, useWorkbenchStore, type TabState, type WorkbenchState } from "../../store/workbenchStore";
import { setMissionClient } from "./clientAccess";
import { formatMissionUpdatedAt, MissionList, missionMatchesQuery } from "./MissionList";
import { click, fakeMission, flushAsync, renderUi, resetAllMissionState, setValue } from "./testSupport";
import { useMissionStore } from "./store";

interface FakeListClient {
  lists: { active: Mission[]; archived: Mission[] };
  listCalls: MissionListParams[];
  controlCalls: MissionControlParams[];
}

function installListClient(active: Mission[], archived: Mission[] = [], extra: Record<string, unknown> = {}): FakeListClient {
  const fake: FakeListClient = { lists: { active, archived }, listCalls: [], controlCalls: [] };
  const client = {
    missionList: async (params: MissionListParams) => {
      fake.listCalls.push(params);
      return { items: params.archived ? fake.lists.archived : fake.lists.active, next_cursor: null };
    },
    missionControl: async (params: MissionControlParams): Promise<MutationResult> => {
      fake.controlCalls.push(params);
      const target = fake.lists.archived.find((mission) => mission.id === params.mission_id);
      const revision = String(Number(target?.revision ?? "0") + 1);
      if (target) {
        fake.lists.archived = fake.lists.archived.filter((mission) => mission.id !== target.id);
        fake.lists.active = [...fake.lists.active, { ...target, archived_at: null, revision }];
      }
      return { mission_id: params.mission_id, revision, event_seq: "9", entity_ids: [params.mission_id] };
    },
    ...extra,
  };
  setMissionClient(client as unknown as DaemonClient);
  return fake;
}

function resetWorkbench(patch: Partial<WorkbenchState> = {}): void {
  useWorkbenchStore.setState({
    tabs: [],
    activeTabId: null,
    focusedLeafId: null,
    panes: {},
    modal: { kind: "mission-list" },
    toast: null,
    hiddenMissions: new Set<string>(),
    missionProtocol: 1,
    ...patch,
  } satisfies Partial<WorkbenchState>);
}

function rows(container: HTMLElement): HTMLElement[] {
  return [...container.querySelectorAll<HTMLElement>("[data-testid=mission-list-row]")];
}

beforeEach(() => {
  resetAllMissionState();
  resetWorkbench();
});

afterEach(() => {
  resetWorkbench({ modal: null });
});

describe("MissionList 그룹과 행", () => {
  it("결정 필요 → 확정 대기 → 진행 중 → 끝남 순으로 묶고 행에 제목·저장소·상태·결정 수를 보인다", async () => {
    const decision = fakeMission({ title: "결제 연동", repository_path: "/work/shop", open_decision_count: 2, updated_at: "2026-09-13T03:00:00.000Z" });
    const acceptance = fakeMission({ title: "로그인", phase: "awaiting_acceptance", repository_path: "/work/auth" });
    const running = fakeMission({ title: "검색 개선", repository_path: "/work/search/" });
    const failed = fakeMission({ title: "배포 스크립트", state: "failed", phase: "done", open_decision_count: 1 });
    installListClient([running, failed, acceptance, decision]);
    const ui = renderUi(<MissionList onClose={() => undefined} />);
    await flushAsync();

    const groups = [...ui.container.querySelectorAll<HTMLElement>(".mission-list-section")].map((section) => section.dataset.testid);
    expect(groups).toEqual([
      "mission-list-group-decision",
      "mission-list-group-acceptance",
      "mission-list-group-active",
      "mission-list-group-finished",
    ]);
    const decisionRow = ui.container.querySelector<HTMLElement>(`[data-mission-id="${decision.id}"]`)!;
    const text = decisionRow.textContent ?? "";
    expect(text).toContain("결제 연동");
    expect(text).toContain("shop");
    expect(text).toContain(t("missions.missionState.running"));
    expect(text).toContain(t("missions.phase.implementing"));
    expect(text).toContain(t("missions.list.decisions", { count: 2 }));
    // 끝난 작업은 남은 결정 수를 "결정 필요"로 부르지 않는다.
    const failedRow = ui.container.querySelector<HTMLElement>(`[data-mission-id="${failed.id}"]`)!;
    expect(failedRow.textContent).not.toContain(t("missions.list.decisions", { count: 1 }));
    // 저장소 끝의 구분자를 떼고 이름만.
    expect(ui.container.querySelector(`[data-mission-id="${running.id}"] .mission-list-repo`)?.textContent).toBe("search");
    // 목록 요약은 store에도 들어가 열린 탭 제목·배지를 채운다.
    expect(useMissionStore.getState().missions[decision.id]?.title).toBe("결제 연동");
    ui.unmount();
  });

  it("필터는 그 묶음만 보이고, 검색은 제목·저장소로 거른다", async () => {
    const decision = fakeMission({ title: "결제 연동", repository_path: "/work/shop", open_decision_count: 1 });
    const running = fakeMission({ title: "검색 개선", repository_path: "/work/search" });
    installListClient([decision, running]);
    const ui = renderUi(<MissionList onClose={() => undefined} />);
    await flushAsync();

    click(ui.container.querySelector("[data-testid=mission-list-filter-decision]")!);
    expect(rows(ui.container).map((row) => row.dataset.missionId)).toEqual([decision.id]);

    click(ui.container.querySelector("[data-testid=mission-list-filter-all]")!);
    setValue(ui.container.querySelector<HTMLInputElement>("[data-testid=mission-list-search]")!, "SEARCH");
    expect(rows(ui.container).map((row) => row.dataset.missionId)).toEqual([running.id]);
    setValue(ui.container.querySelector<HTMLInputElement>("[data-testid=mission-list-search]")!, "shop");
    expect(rows(ui.container).map((row) => row.dataset.missionId)).toEqual([decision.id]);
    setValue(ui.container.querySelector<HTMLInputElement>("[data-testid=mission-list-search]")!, "없는 작업");
    expect(rows(ui.container)).toHaveLength(0);
    expect(ui.container.textContent).toContain(t("missions.list.noMatch"));
    ui.unmount();
  });
});

describe("MissionList 탭 열기", () => {
  it("행을 누르면 작업 탭을 열고 대화상자를 닫는다", async () => {
    const mission = fakeMission({ title: "검색 개선" });
    installListClient([mission]);
    let closed = 0;
    const ui = renderUi(<MissionList onClose={() => { closed += 1; }} />);
    await flushAsync();
    click(ui.container.querySelector(`[data-mission-id="${mission.id}"] .mission-list-open`)!);
    const state = useWorkbenchStore.getState();
    expect(state.tabs).toHaveLength(1);
    expect(state.tabs[0]).toMatchObject({ kind: "mission", missionId: mission.id });
    expect(state.activeTabId).toBe(state.tabs[0].id);
    expect(closed).toBe(1);
    ui.unmount();
  });

  it("이미 열린 탭이면 새 탭 없이 그 탭으로 이동하고 '열린 탭' 표시가 있다", async () => {
    const mission = fakeMission({ title: "검색 개선" });
    const existing: TabState = { kind: "mission", id: "tab-existing", title: "", missionId: mission.id };
    resetWorkbench({ tabs: [{ kind: "terminal", id: "tab-term", title: "zsh", root: null }, existing], activeTabId: "tab-term" });
    installListClient([mission]);
    const ui = renderUi(<MissionList onClose={() => undefined} />);
    await flushAsync();
    expect(ui.container.querySelector(`[data-mission-id="${mission.id}"]`)?.textContent).toContain(t("missions.list.tabOpen"));
    click(ui.container.querySelector(`[data-mission-id="${mission.id}"] .mission-list-open`)!);
    expect(useWorkbenchStore.getState().tabs).toHaveLength(2);
    expect(useWorkbenchStore.getState().activeTabId).toBe("tab-existing");
    ui.unmount();
  });

  it("탭 상한이면 대화상자 안에 안내하고 닫지 않는다(토스트로 덮지 않는다)", async () => {
    const tabs: TabState[] = Array.from({ length: MAX_MISSION_TABS }, (_, index) => ({
      kind: "mission",
      id: `tab-${index}`,
      title: "",
      missionId: `other-${index}`,
    }));
    resetWorkbench({ tabs, activeTabId: "tab-0" });
    const mission = fakeMission({ title: "새로 열 작업" });
    installListClient([mission]);
    let closed = 0;
    const ui = renderUi(<MissionList onClose={() => { closed += 1; }} />);
    await flushAsync();
    click(ui.container.querySelector(`[data-mission-id="${mission.id}"] .mission-list-open`)!);
    expect(closed).toBe(0);
    expect(useWorkbenchStore.getState().tabs).toHaveLength(MAX_MISSION_TABS);
    expect(ui.container.querySelector("[data-testid=mission-list-tab-limit]")?.textContent).toBe(
      t("missions.list.tabLimit", { count: MAX_MISSION_TABS }),
    );
    expect(useWorkbenchStore.getState().toast).toBeNull();
    ui.unmount();
  });
});

describe("MissionList 빈 상태·보관", () => {
  it("작업이 하나도 없으면 빈 상태 문구와 새 AI 작업 버튼을 보인다", async () => {
    installListClient([]);
    const ui = renderUi(<MissionList onClose={() => undefined} />);
    await flushAsync();
    const empty = ui.container.querySelector("[data-testid=mission-list-empty]");
    expect(empty?.textContent).toContain(t("missions.list.empty"));
    const button = empty?.querySelector<HTMLButtonElement>("[data-testid=mission-list-new]");
    expect(button?.disabled).toBe(false);
    click(button!);
    expect(useWorkbenchStore.getState().missionCreate).toMatchObject({ kind: "mission-create" });
    ui.unmount();
  });

  it("데몬이 프로토콜을 선언하지 않으면(개발 빌드) 새 AI 작업 버튼은 사유와 함께 비활성", async () => {
    resetWorkbench({ missionProtocol: null });
    installListClient([]);
    const ui = renderUi(<MissionList onClose={() => undefined} />);
    await flushAsync();
    const button = ui.container.querySelector<HTMLButtonElement>("[data-testid=mission-list-new]");
    expect(button?.disabled).toBe(true);
    expect(button?.title).toBe(t("missions.newMission.unavailable"));
    ui.unmount();
  });

  it("보관됨은 고를 때 archived:true로 읽고, 보관 해제는 최신 revision으로 unarchive를 보낸 뒤 다시 읽는다", async () => {
    const archived = fakeMission({ title: "지난 작업", state: "completed", phase: "done", revision: "4", archived_at: "2026-09-13T02:00:00.000Z" });
    const fake = installListClient([], [archived]);
    const ui = renderUi(<MissionList onClose={() => undefined} />);
    await flushAsync();
    expect(fake.listCalls.map((call) => call.archived)).toEqual([false]);

    click(ui.container.querySelector("[data-testid=mission-list-filter-archived]")!);
    await flushAsync();
    expect(fake.listCalls.map((call) => call.archived)).toEqual([false, true]);
    expect(rows(ui.container).map((row) => row.dataset.missionId)).toEqual([archived.id]);

    click(ui.container.querySelector("[data-testid=mission-list-unarchive]")!);
    await flushAsync();
    expect(fake.controlCalls).toHaveLength(1);
    expect(fake.controlCalls[0]).toMatchObject({ mission_id: archived.id, expected_revision: "4", action: "unarchive" });
    // 다시 읽은 목록에서 보관됨에는 없고, 전체에는 끝남으로 보인다.
    expect(rows(ui.container)).toHaveLength(0);
    click(ui.container.querySelector("[data-testid=mission-list-filter-all]")!);
    expect(ui.container.querySelector(`[data-testid=mission-list-group-finished] [data-mission-id="${archived.id}"]`)).not.toBeNull();
    ui.unmount();
  });
});

describe("MissionList 다시 그리기", () => {
  it("검색 입력·언어 전환으로 다시 그려도 목록을 다시 읽지 않는다(불러오기 함수가 렌더마다 바뀌지 않는다)", async () => {
    const archived = fakeMission({ title: "지난 작업", state: "completed", phase: "done", archived_at: "2026-09-13T02:00:00.000Z" });
    const fake = installListClient([fakeMission({ title: "진행 작업" })], [archived]);
    const ui = renderUi(<MissionList onClose={() => undefined} />);
    try {
      await flushAsync();
      click(ui.container.querySelector("[data-testid=mission-list-filter-archived]")!);
      await flushAsync();
      expect(fake.listCalls.map((call) => call.archived)).toEqual([false, true]);
      for (const text of ["지", "지난", ""]) {
        setValue(ui.container.querySelector<HTMLInputElement>("[data-testid=mission-list-search]")!, text);
        await flushAsync(1);
      }
      act(() => useI18nStore.setState({ language: "en" }));
      await flushAsync(2);
      expect(fake.listCalls.map((call) => call.archived)).toEqual([false, true]);
      expect(rows(ui.container).map((row) => row.dataset.missionId)).toEqual([archived.id]);
    } finally {
      ui.unmount();
      useI18nStore.setState({ language: "ko" });
    }
  });
});

describe("MissionList 작업 공간 정리(계약 D)", () => {
  const MB = 1024 * 1024;

  function row(container: HTMLElement, missionId: string): HTMLElement {
    return container.querySelector<HTMLElement>(`[data-mission-id="${missionId}"]`)!;
  }

  it("열 때 전체 사용량을 보이고, 끝남 행은 정리 가능하면 버튼·막히면 짧은 이유를 보인다(진행 중 행에는 없음)", async () => {
    const running = fakeMission({ title: "검색 개선" });
    const cleanable = fakeMission({ title: "배포 스크립트", state: "failed", phase: "done" });
    const blocked = fakeMission({ title: "로그 정리", state: "cancelled", phase: "done" });
    const usage: WorkspaceUsageResult = {
      missions: [
        { mission_id: running.id, workspaces: 1, bytes: String(MB), cleanable: false, blocked_reason: "mission_active" },
        { mission_id: cleanable.id, workspaces: 2, bytes: String(3 * MB), cleanable: true, blocked_reason: null },
        { mission_id: blocked.id, workspaces: 1, bytes: String(MB), cleanable: false, blocked_reason: "run_unreconciled" },
      ],
      total_bytes: String(5 * MB),
    };
    const workspaceUsage = vi.fn(async (_params: WorkspaceUsageParams) => usage);
    installListClient([running, cleanable, blocked], [], { workspaceUsage });
    const ui = renderUi(<MissionList onClose={() => undefined} />);
    await flushAsync();

    expect(workspaceUsage).toHaveBeenCalledTimes(1);
    expect(workspaceUsage.mock.calls[0][0]).toEqual({ mission_id: null });
    expect(ui.container.querySelector("[data-testid=mission-list-workspace-total]")?.textContent)
      .toBe(t("missions.workspaceCleanup.total", { size: "5 MB" }));

    expect(row(ui.container, cleanable.id).querySelector("[data-testid=workspace-cleanup-button]")?.textContent)
      .toBe(t("missions.workspaceCleanup.action", { size: "3 MB" }));
    const blockedRow = row(ui.container, blocked.id);
    expect(blockedRow.querySelector("[data-testid=workspace-cleanup-button]")).toBeNull();
    expect(blockedRow.querySelector("[data-testid=workspace-cleanup-blocked]")?.textContent)
      .toContain(t("missions.workspaceCleanup.blocked.runUnreconciled"));
    expect(blockedRow.textContent).not.toContain("run_unreconciled");
    expect(row(ui.container, running.id).querySelector("[data-testid=workspace-cleanup]")).toBeNull();
    ui.unmount();
  });

  it("정리는 확인창을 거쳐 보내고, 제거 수·확보 용량·남긴 항목과 이유를 보인 뒤 사용량을 다시 읽는다", async () => {
    const archived = fakeMission({ title: "지난 작업", state: "completed", phase: "done", archived_at: "2026-09-13T02:00:00.000Z" });
    const usage: WorkspaceUsageResult = {
      missions: [{ mission_id: archived.id, workspaces: 2, bytes: String(4 * MB), cleanable: true, blocked_reason: null }],
      total_bytes: String(4 * MB),
    };
    const workspaceUsage = vi.fn(async (_params: WorkspaceUsageParams) => usage);
    const workspaceCleanup = vi.fn(async (_params: WorkspaceCleanupParams): Promise<WorkspaceCleanupResult> => ({
      removed: 1,
      freed_bytes: String(2 * MB),
      kept: [{ path: "/tmp/iyagi/worktrees/w-2", reason: "dirty" }],
    }));
    installListClient([], [archived], { workspaceUsage, workspaceCleanup });
    const ui = renderUi(<MissionList onClose={() => undefined} />);
    await flushAsync();
    click(ui.container.querySelector("[data-testid=mission-list-filter-archived]")!);
    await flushAsync();

    const target = row(ui.container, archived.id);
    click(target.querySelector("[data-testid=workspace-cleanup-button]")!);
    const confirm = target.querySelector("[data-testid=workspace-cleanup-confirm]")!;
    expect(confirm.getAttribute("role")).toBe("alertdialog");
    expect(confirm.textContent).toContain(t("missions.workspaceCleanup.confirmBody"));
    expect(workspaceCleanup).not.toHaveBeenCalled();
    click(target.querySelector("[data-testid=workspace-cleanup-cancel]")!);
    expect(target.querySelector("[data-testid=workspace-cleanup-confirm]")).toBeNull();
    expect(workspaceCleanup).not.toHaveBeenCalled();

    click(target.querySelector("[data-testid=workspace-cleanup-button]")!);
    click(target.querySelector("[data-testid=workspace-cleanup-ok]")!);
    await flushAsync();
    expect(workspaceCleanup).toHaveBeenCalledTimes(1);
    expect(workspaceCleanup.mock.calls[0][0]).toEqual({ request_id: expect.any(String), mission_id: archived.id });

    const result = row(ui.container, archived.id).querySelector("[data-testid=workspace-cleanup-result]")!;
    expect(result.textContent).toContain(t("missions.workspaceCleanup.done", { removed: 1, size: "2 MB" }));
    expect(result.textContent).toContain(t("missions.workspaceCleanup.keptTitle", { count: 1 }));
    const kept = result.querySelector("[data-testid=workspace-kept-item]")!;
    expect(kept.textContent).toContain("/tmp/iyagi/worktrees/w-2");
    expect(kept.textContent).toContain(t("missions.workspaceCleanup.kept.dirty"));
    expect(kept.textContent).not.toContain("· dirty");
    expect(workspaceUsage).toHaveBeenCalledTimes(2);
    ui.unmount();
  });

  it("사용량 조회 실패는 조용히 숨기고, 정리 실패는 원인 문장 알림으로 보인다", async () => {
    const finished = fakeMission({ title: "배포 스크립트", state: "failed", phase: "done" });
    const failingUsage = vi.fn(async (_params: WorkspaceUsageParams): Promise<WorkspaceUsageResult> => {
      throw new RpcClientError("INTERNAL", "usage failed");
    });
    installListClient([finished], [], { workspaceUsage: failingUsage });
    let ui = renderUi(<MissionList onClose={() => undefined} />);
    await flushAsync();
    expect(failingUsage).toHaveBeenCalledTimes(1);
    expect(ui.container.querySelector("[data-testid=mission-list-workspace-total]")).toBeNull();
    expect(ui.container.querySelector("[data-testid=workspace-cleanup]")).toBeNull();
    expect(ui.container.querySelector("[role=alert]")).toBeNull();
    ui.unmount();

    const workspaceUsage = vi.fn(async (_params: WorkspaceUsageParams): Promise<WorkspaceUsageResult> => ({
      missions: [{ mission_id: finished.id, workspaces: 1, bytes: String(MB), cleanable: true, blocked_reason: null }],
      total_bytes: String(MB),
    }));
    const workspaceCleanup = vi.fn(async (_params: WorkspaceCleanupParams): Promise<WorkspaceCleanupResult> => {
      throw new RpcClientError("WORKSPACE_BUSY", "worktree locked");
    });
    installListClient([finished], [], { workspaceUsage, workspaceCleanup });
    ui = renderUi(<MissionList onClose={() => undefined} />);
    await flushAsync();
    click(ui.container.querySelector("[data-testid=workspace-cleanup-button]")!);
    click(ui.container.querySelector("[data-testid=workspace-cleanup-ok]")!);
    await flushAsync();
    const alerts = ui.container.querySelectorAll("[role=alert]");
    expect(alerts).toHaveLength(1);
    expect(alerts[0].querySelector("[data-testid=mission-error-message]")?.textContent).toBe(t("missions.error.code.workspaceBusy"));
    expect(ui.container.querySelector("[data-testid=workspace-cleanup-result]")).toBeNull();
    // 다시 시도는 확인창부터 다시 받는다.
    click(alerts[0].querySelector("[data-testid=mission-error-action]")!);
    expect(ui.container.querySelector("[data-testid=workspace-cleanup-confirm]")).not.toBeNull();
    expect(workspaceCleanup).toHaveBeenCalledTimes(1);
    ui.unmount();
  });
});

describe("MissionList 순수 도우미", () => {
  it("검색어는 제목·저장소 경로·저장소 이름에 대소문자 없이 맞춘다", () => {
    const mission = { title: "Login Flow", repository_path: "/Users/me/Shop" };
    expect(missionMatchesQuery(mission, "  login ")).toBe(true);
    expect(missionMatchesQuery(mission, "shop")).toBe(true);
    expect(missionMatchesQuery(mission, "cart")).toBe(false);
    expect(missionMatchesQuery(mission, "")).toBe(true);
  });

  it("마지막 갱신은 오늘이면 시각만, 올해면 월-일까지, 그 밖은 연도까지", () => {
    const now = new Date(2026, 8, 17, 12, 0);
    expect(formatMissionUpdatedAt(new Date(2026, 8, 17, 9, 5).toISOString(), now)).toBe("09:05");
    expect(formatMissionUpdatedAt(new Date(2026, 0, 2, 9, 5).toISOString(), now)).toBe("01-02 09:05");
    expect(formatMissionUpdatedAt(new Date(2025, 11, 31, 23, 59).toISOString(), now)).toBe("2025-12-31 23:59");
    expect(formatMissionUpdatedAt("not a date", now)).toBe("");
  });
});
