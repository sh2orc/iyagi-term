/**
 * MissionPage 시험(05 §1·§7·§8·§9·§10): 결과 화면 자동 전환, 결정 배너의 다음 결정
 * 이동, 멈추는 중 제어 메뉴, 보관·되돌리기, 기록 없음 탭 닫기, 연결 끊김과 동기화
 * 지연 구분, 빈 선택 자동 채우기.
 *
 * 무거운 하위 화면(실행 상세·결과 검토·결정 패널·한도 편집기)은 자체 시험이 있어
 * 자리와 전달된 값만 남기는 대역으로 바꾼다.
 */

import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Decision } from "../../generated/Decision";
import type { Mission } from "../../generated/Mission";
import { RpcClientError } from "../daemon/client";
import { t } from "../../i18n";
import { useWorkbenchStore } from "../../store/workbenchStore";
import { MissionPage } from "./MissionPage";
import { dismissArchiveUndoToast } from "./ArchiveUndoToast";
import { applyConnection, resetMissionConnectionForTests, useMissionConnectionStore } from "./missionConnection";
import { useMissionStore } from "./store";
import { useMissionUiStore } from "./uiStore";
import {
  click,
  fakeDecision,
  fakeMission,
  fakeRun,
  fakeTask,
  flushAsync,
  installMockClient,
  renderUi,
  resetAllMissionState,
  seedStore,
} from "./testSupport";

/** 결과 화면 대역의 링크가 가리킬 대상(시험마다 채운다). */
const resultLinkTargets = vi.hoisted(() => ({ taskId: "", decisionId: "" }));

vi.mock("./RunDetail", async () => {
  const { createElement } = await import("react");
  return {
    RunDetail: (props: { task: { id: string; title: string } }) =>
      createElement("div", { "data-testid": "run-detail", "data-task-id": props.task.id }, props.task.title),
  };
});

vi.mock("./ResultReview", async () => {
  const { createElement } = await import("react");
  type Props = {
    onOpenTask?: (taskId: string) => void;
    onOpenDecision?: (decisionId: string) => void;
    onRequestChanges?: () => void;
    onArchived?: () => void;
  };
  const link = (testId: string, run: (() => void) | undefined) =>
    run ? createElement("button", { type: "button", "data-testid": testId, onClick: run }, testId) : null;
  return {
    ResultReview: (props: Props & { mission: { id: string } }) =>
      createElement(
        "div",
        { "data-testid": "result-review" },
        link("result-open-task", props.onOpenTask && (() => props.onOpenTask?.(resultLinkTargets.taskId))),
        link("result-open-decision", props.onOpenDecision && (() => props.onOpenDecision?.(resultLinkTargets.decisionId))),
        link("result-request-changes", props.onRequestChanges),
        link("result-archived", props.onArchived),
      ),
  };
});


vi.mock("./DecisionPanel", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./DecisionPanel")>();
  const { createElement } = await import("react");
  return {
    ...actual,
    DecisionPanel: (props: { decision: Decision; onOpenTask?: (taskId: string) => void }) =>
      createElement(
        "section",
        { "data-testid": "decision-panel", "data-decision-id": props.decision.id },
        createElement(
          "button",
          {
            type: "button",
            "data-testid": "panel-open-task",
            onClick: () => props.onOpenTask?.(props.decision.affected_task_ids[0] ?? ""),
          },
          "open",
        ),
      ),
  };
});

vi.mock("./CostPolicyEditor", async () => {
  const { createElement } = await import("react");
  return {
    PolicyLimitsEditor: () => createElement("div", { "data-testid": "policy-limits-editor" }),
    CostPolicyEditor: () => null,
  };
});

const TAB_ID = "tab-mission";

function setViewportWidth(width: number): void {
  Object.defineProperty(window, "innerWidth", { configurable: true, value: width });
}

function openTab(mission: Mission): void {
  useWorkbenchStore.setState({
    tabs: [{ kind: "mission", id: TAB_ID, title: mission.title, missionId: mission.id }],
    activeTabId: TAB_ID,
    modal: null,
    toast: null,
  });
}

function query(container: ParentNode, testId: string): HTMLElement | null {
  return container.querySelector<HTMLElement>(`[data-testid='${testId}']`);
}

function ui(missionId: string) {
  return useMissionUiStore.getState().getUi(missionId);
}

const realSyncMission = useMissionStore.getState().syncMission;

/** 동기화를 대역으로 바꾼다(데몬 없이 결과만 store에 반영). 호출된 mission id를 모은다. */
function fakeSync(apply: (missionId: string) => void = () => undefined): string[] {
  const calls: string[] = [];
  useMissionStore.setState({
    syncMission: async (missionId: string) => {
      calls.push(missionId);
      apply(missionId);
    },
  });
  return calls;
}

function setSyncError(missionId: string, error: string): void {
  useMissionStore.setState((state) => ({
    sync: { ...state.sync, [missionId]: { atSeq: "1", revision: "1", loading: false, error, dirty: false, hintSeq: null } },
  }));
}

beforeEach(() => {
  resetAllMissionState();
  useMissionStore.setState({ syncMission: realSyncMission });
  resetMissionConnectionForTests();
  setViewportWidth(1280);
  useWorkbenchStore.setState({ tabs: [], activeTabId: null, modal: null, toast: null });
});

afterEach(async () => {
  useMissionStore.setState({ syncMission: realSyncMission });
  dismissArchiveUndoToast();
  await flushAsync(1);
});

describe("결과 화면 자동 전환(05 §9)", () => {
  it("인수 대기로 바뀌는 순간 한 번 결과로 전환하고, 그 뒤 사용자 선택을 존중한다", () => {
    const mission = fakeMission({ phase: "implementing" });
    seedStore({ missions: [mission] });
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    expect(query(handle.container, "toggle-detail")?.getAttribute("aria-pressed")).toBe("true");
    expect(query(handle.container, "result-pending-dot")).toBeNull();

    act(() => seedStore({ missions: [{ ...mission, phase: "awaiting_acceptance", revision: "6" }] }));
    expect(query(handle.container, "toggle-result")?.getAttribute("aria-pressed")).toBe("true");
    expect(query(handle.container, "result-review")).not.toBeNull();
    // 결과 토글에 확정 대기 점.
    expect(query(query(handle.container, "toggle-result")!, "result-pending-dot")?.textContent).toBe(
      t("missions.page.resultPending"),
    );

    click(query(handle.container, "toggle-detail")!);
    act(() => seedStore({ missions: [{ ...mission, phase: "awaiting_acceptance", revision: "7" }] }));
    expect(query(handle.container, "toggle-detail")?.getAttribute("aria-pressed")).toBe("true");
    expect(ui(mission.id).rightPane).toBe("detail");
    handle.unmount();
  });

  it("좁은 화면에서도 결과 탭으로 전환한다", () => {
    setViewportWidth(700);
    const mission = fakeMission({ phase: "reviewing" });
    seedStore({ missions: [mission] });
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    expect(query(handle.container, "toggle-detail")?.getAttribute("aria-pressed")).toBe("true");
    act(() => seedStore({ missions: [{ ...mission, phase: "awaiting_acceptance" }] }));
    expect(query(handle.container, "toggle-result")?.getAttribute("aria-pressed")).toBe("true");
    expect(query(handle.container, "result-review")).not.toBeNull();
    expect(query(query(handle.container, "toggle-result")!, "result-pending-dot")).not.toBeNull();
    handle.unmount();
  });
});

describe("결과 화면 링크 연결", () => {
  function seedResult() {
    const mission = fakeMission({ phase: "awaiting_acceptance" });
    const task = fakeTask(mission.id, "succeeded", { title: "검토 대상" });
    const decision = fakeDecision(mission.id, { kind: "budget", blocking: true });
    seedStore({ missions: [mission], tasks: [task], decisions: [decision] });
    resultLinkTargets.taskId = task.id;
    resultLinkTargets.decisionId = decision.id;
    return { mission, task, decision };
  }

  it.each([1280, 700])("%ipx: 할 일·결정 링크는 상세와 결정 패널을 연다", (width) => {
    setViewportWidth(width);
    const { mission, task, decision } = seedResult();
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    click(query(handle.container, "result-open-decision")!);
    expect(query(handle.container, "decision-panel")?.getAttribute("data-decision-id")).toBe(decision.id);
    click(query(handle.container, "result-open-task")!);
    expect(ui(mission.id)).toMatchObject({ selectedTaskId: task.id, rightPane: "detail", sheetOpen: true });
    expect(query(handle.container, "run-detail")?.getAttribute("data-task-id")).toBe(task.id);
    handle.unmount();
  });

  it("수정 요청은 좁은 화면에서 대화 영역으로 바꾸고 입력창에 초점을 준다", async () => {
    setViewportWidth(700);
    const { mission } = seedResult();
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    expect(query(handle.container, "toggle-result")?.getAttribute("aria-pressed")).toBe("true");
    click(query(handle.container, "result-request-changes")!);
    await flushAsync(2);
    expect(query(handle.container, "toggle-lead")?.getAttribute("aria-pressed")).toBe("true");
    // 입력창은 초안 복원이 끝나야 활성화된다 — 그 뒤에 초점이 간다.
    await vi.waitFor(() => expect(document.activeElement).toBe(query(handle.container, "lead-composer")), { timeout: 2000 });
    handle.unmount();
  });

  it("결과 버리기로 보관되면 보관과 같은 경로로 탭을 닫고 되돌리기 토스트를 띄운다", async () => {
    const { mission } = seedResult();
    openTab(mission);
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    click(query(handle.container, "result-archived")!);
    await flushAsync(1);
    expect(useWorkbenchStore.getState().tabs.some((tab) => tab.id === TAB_ID)).toBe(false);
    expect(query(document.body, "archive-undo-toast")).not.toBeNull();
    handle.unmount();
  });
});

describe("결정 배너(05 §8)", () => {
  function seedDecisions() {
    const mission = fakeMission();
    const task = fakeTask(mission.id, "blocked", { title: "계획 검토 대상" });
    const first = fakeDecision(mission.id, { kind: "plan", created_at: "2026-09-13T01:00:00.000Z", affected_task_ids: [task.id] });
    const second = fakeDecision(mission.id, { kind: "product", created_at: "2026-09-13T01:05:00.000Z" });
    seedStore({ missions: [mission], tasks: [task], decisions: [first, second] });
    return { mission, task, first, second };
  }

  it("종류별 짧은 제목을 보이고 열기/접기 라벨을 바꾼다", () => {
    const { mission, first } = seedDecisions();
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    const text = query(handle.container, "decision-banner-text");
    expect(text?.textContent).toBe(t("missions.banner.decision", { count: 2, question: t("missions.banner.kind.plan") }));
    expect(text?.getAttribute("aria-live")).toBe("polite");
    // 낭독 영역은 배너 한 곳뿐이다.
    expect(handle.container.querySelectorAll("[aria-live]").length).toBe(1);

    const toggle = query(handle.container, "decision-banner-open")!;
    expect(toggle.textContent).toBe(t("missions.banner.open"));
    click(toggle);
    expect(toggle.textContent).toBe(t("missions.banner.close"));
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(query(handle.container, "decision-panel")?.getAttribute("data-decision-id")).toBe(first.id);
    click(toggle);
    expect(query(handle.container, "decision-panel")).toBeNull();
    expect(toggle.textContent).toBe(t("missions.banner.open"));
    handle.unmount();
  });

  it("답하면 다음 열린 결정으로 넘어가고, 마지막 답 뒤에는 배너를 닫고 저장만 알린다", () => {
    const { mission, first, second } = seedDecisions();
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    click(query(handle.container, "decision-banner-open")!);

    act(() => seedStore({ decisions: [{ ...first, state: "answered", selected_option_id: "keep" }] }));
    expect(query(handle.container, "decision-panel")?.getAttribute("data-decision-id")).toBe(second.id);
    const text = query(handle.container, "decision-banner-text")?.textContent ?? "";
    expect(text).toContain(t("missions.banner.saved"));
    expect(text).toContain(t("missions.banner.decision", { count: 1, question: t("missions.banner.kind.product") }));

    act(() => seedStore({ decisions: [{ ...second, state: "answered", selected_option_id: "keep" }] }));
    expect(query(handle.container, "decision-banner")).toBeNull();
    expect(query(handle.container, "decision-panel")).toBeNull();
    expect(query(handle.container, "decision-saved")?.textContent).toBe(t("missions.banner.saved"));
    handle.unmount();
  });

  it("결정 패널의 할 일 링크는 그 할 일을 선택하고 상세를 연다", () => {
    const { mission, task } = seedDecisions();
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    click(query(handle.container, "decision-banner-open")!);
    click(query(handle.container, "panel-open-task")!);
    expect(ui(mission.id)).toMatchObject({ selectedTaskId: task.id, rightPane: "detail", sheetOpen: true, selectedByUser: true });
    expect(query(handle.container, "run-detail")?.getAttribute("data-task-id")).toBe(task.id);
    handle.unmount();
  });
});

describe("실행 제어 메뉴(05 §8)", () => {
  it("멈추는 중에는 제어 대신 종료 확인을 기다리는 실행과 첫 실행 열기를 보인다", () => {
    const mission = fakeMission({ state: "stopping" });
    const task = fakeTask(mission.id, "running", { title: "멈추는 실행" });
    const run = fakeRun(mission.id, task, "stopping");
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    click(query(handle.container, "mission-control")!);
    const menu = query(handle.container, "mission-control-menu")!;
    expect(query(menu, "control-waiting")?.textContent).toBe(t("missions.control.waitingRuns", { count: 1 }));
    expect(query(menu, "mission-cancel")).toBeNull();
    expect(query(menu, "mission-pause")).toBeNull();
    click(query(menu, "control-open-first-run")!);
    expect(ui(mission.id)).toMatchObject({ selectedTaskId: task.id, rightPane: "detail" });
    expect(query(handle.container, "mission-control-menu")).toBeNull();
    handle.unmount();
  });

  it("메뉴에서 이 작업의 한도 편집기를 연다", () => {
    const mission = fakeMission({ state: "running" });
    seedStore({ missions: [mission] });
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    click(query(handle.container, "mission-control")!);
    click(query(handle.container, "mission-limits")!);
    expect(query(handle.container, "policy-limits-editor")).not.toBeNull();
    handle.unmount();
  });

  it("메뉴에서 이 작업의 팀 편집기를 연다", () => {
    const mission = fakeMission({ state: "running" });
    seedStore({ missions: [mission] });
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    click(query(handle.container, "mission-control")!);
    click(query(handle.container, "mission-team")!);
    expect(query(handle.container, "mission-team-editor")).not.toBeNull();
    handle.unmount();
  });

  it("일시정지는 최신 revision으로 보내고 충돌이면 다시 동기화 뒤 한 번 더 보낸다", async () => {
    const client = installMockClient();
    const mission = fakeMission({ state: "running", revision: "5" });
    seedStore({ missions: [mission] });
    const control = vi
      .spyOn(client, "missionControl")
      .mockRejectedValueOnce(new RpcClientError("REVISION_CONFLICT", "stale"))
      .mockResolvedValueOnce({ mission_id: mission.id, revision: "7", event_seq: "1", entity_ids: [] });
    let daemonRevision = "5";
    fakeSync(() => seedStore({ missions: [{ ...mission, revision: daemonRevision }] }));
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    await flushAsync(3);
    // 화면이 모르는 사이 데몬 revision이 올라갔다.
    daemonRevision = "6";
    click(query(handle.container, "mission-control")!);
    click(query(handle.container, "mission-pause")!);
    await flushAsync(6);
    expect(control).toHaveBeenCalledTimes(2);
    expect(control.mock.calls[0][0]).toMatchObject({ action: "pause", expected_revision: "5" });
    expect(control.mock.calls[1][0]).toMatchObject({ action: "pause", expected_revision: "6" });
    expect(control.mock.calls[0][0].request_id).not.toBe(control.mock.calls[1][0].request_id);
    expect(query(handle.container, "mission-control-error")).toBeNull();
    handle.unmount();
  });
});

describe("보관(05 §2)", () => {
  it("확인 뒤 보관하고 탭을 닫으며, 되돌리기는 보관을 해제하고 탭을 다시 연다", async () => {
    const client = installMockClient();
    const mission = fakeMission({ state: "completed", phase: "done" });
    seedStore({ missions: [mission] });
    openTab(mission);
    const control = vi
      .spyOn(client, "missionControl")
      .mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "1", entity_ids: [] });
    const handle = renderUi(<MissionPage missionId={mission.id} />);

    click(query(handle.container, "mission-archive")!);
    const confirm = query(handle.container, "mission-archive-confirm")!;
    expect(confirm.textContent).toContain(t("missions.control.archiveBody"));
    expect(control).not.toHaveBeenCalled();
    click(query(confirm, "mission-archive-ok")!);
    await flushAsync(4);
    expect(control.mock.calls[0][0]).toMatchObject({ mission_id: mission.id, action: "archive" });
    expect(useWorkbenchStore.getState().tabs.some((tab) => tab.id === TAB_ID)).toBe(false);

    const toast = query(document.body, "archive-undo-toast")!;
    expect(toast.textContent).toContain(t("missions.control.archived"));
    click(query(toast, "archive-undo")!);
    await flushAsync(4);
    expect(control.mock.calls[1][0]).toMatchObject({ mission_id: mission.id, action: "unarchive" });
    expect(
      useWorkbenchStore.getState().tabs.some((tab) => tab.kind === "mission" && tab.missionId === mission.id),
    ).toBe(true);
    expect(query(document.body, "archive-undo-toast")).toBeNull();
    handle.unmount();
  });

  it("되돌리기 실패 알림(alert)은 토스트의 status 낭독 영역 안에 중첩되지 않는다", async () => {
    const client = installMockClient();
    const mission = fakeMission({ state: "completed", phase: "done" });
    seedStore({ missions: [mission] });
    openTab(mission);
    fakeSync();
    vi.spyOn(client, "missionControl")
      .mockResolvedValueOnce({ mission_id: mission.id, revision: "6", event_seq: "1", entity_ids: [] })
      .mockRejectedValueOnce(new RpcClientError("INTERNAL", "unarchive failed"));
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    click(query(handle.container, "mission-archive")!);
    click(query(query(handle.container, "mission-archive-confirm")!, "mission-archive-ok")!);
    await flushAsync(4);
    const toast = query(document.body, "archive-undo-toast")!;
    expect(toast.getAttribute("role")).toBeNull();
    expect(toast.querySelector("[role=status]")?.textContent).toBe(t("missions.control.archived"));
    click(query(toast, "archive-undo")!);
    await flushAsync(4);
    expect(toast.querySelector("[role=alert]")).not.toBeNull();
    expect(toast.querySelector("[role=status] [role=alert]")).toBeNull();
    handle.unmount();
  });

  it("이미 보관된 작업이면 보관 버튼을 숨긴다", () => {
    const mission = fakeMission({ state: "failed", archived_at: "2026-09-13T02:00:00.000Z" });
    seedStore({ missions: [mission] });
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    expect(query(handle.container, "mission-archive")).toBeNull();
    handle.unmount();
  });
});

describe("기록 없음·연결 상태(05 §10)", () => {
  it("NOT_FOUND 작업 탭은 재시도 대신 탭 닫기를 권한다", async () => {
    const mission = fakeMission();
    openTab(mission);
    fakeSync((missionId) => setSyncError(missionId, "NOT_FOUND: mission not found"));
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    await flushAsync(3);
    expect(query(handle.container, "mission-not-found")?.textContent).toContain(t("missions.notFound.title"));
    expect(handle.container.textContent).not.toContain(t("missions.sync.retry"));
    click(query(handle.container, "not-found-close")!);
    expect(useWorkbenchStore.getState().tabs).toEqual([]);
    handle.unmount();
  });

  it("일시 오류는 연결 끊김이 아니라 동기화 지연으로 표시한다", async () => {
    const mission = fakeMission();
    seedStore({ missions: [mission] });
    const calls = fakeSync((missionId) => setSyncError(missionId, "BUSY: daemon busy"));
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    await flushAsync(3);
    expect(query(handle.container, "sync-delayed")?.textContent).toContain(t("missions.sync.delayed"));
    expect(query(handle.container, "connection-lost")).toBeNull();
    expect(handle.container.textContent).not.toContain(t("missions.disconnected"));
    const before = calls.length;
    click(query(handle.container, "sync-retry")!);
    await flushAsync(3);
    expect(calls.length).toBe(before + 1);
    handle.unmount();
  });

  it("연결이 끊기면 흐리게 하고 변경 요청을 막으며, 재연결되면 다시 동기화한다", async () => {
    const mission = fakeMission({ state: "running" });
    const decision = fakeDecision(mission.id);
    seedStore({ missions: [mission], decisions: [decision] });
    const calls = fakeSync();
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    await flushAsync(3);
    const before = calls.length;
    // 기본 우측창은 이제 리드 대화가 아니다(5f30ca9) — 리드 패널로 직접 바꿔야
    // lead-send·composer-offline가 렌더된다.
    act(() => click(query(handle.container, "toggle-lead")!));
    await flushAsync(3);

    act(() => applyConnection("disconnected", 1));
    const page = query(handle.container, "mission-page")!;
    expect(page.classList.contains("offline")).toBe(true);
    expect(query(handle.container, "connection-lost")?.textContent).toBe(t("missions.disconnected"));
    expect((query(handle.container, "mission-control") as HTMLButtonElement).disabled).toBe(true);
    expect((query(handle.container, "decision-banner-open") as HTMLButtonElement).disabled).toBe(true);
    expect((query(handle.container, "lead-send") as HTMLButtonElement).disabled).toBe(true);
    expect(query(handle.container, "composer-offline")?.textContent).toBe(t("missions.composer.lockedOffline"));

    act(() => applyConnection("connected", 2));
    expect(useMissionConnectionStore.getState().reconnects).toBe(1);
    await flushAsync(3);
    expect(calls.length).toBe(before + 1);
    expect(query(handle.container, "connection-lost")).toBeNull();
    expect(page.classList.contains("offline")).toBe(false);
    handle.unmount();
  });
});

describe("동기화 실패 재시도 간격(05 §10)", () => {
  it("dirty인 채 실패하면 곧바로 다시 뽑지 않고 1s→2s→4s로 늘려 기다리며, 성공하면 처음 간격으로 돌아간다", async () => {
    vi.useFakeTimers();
    const mission = fakeMission();
    seedStore({ missions: [mission] });
    let fail = true;
    const calls: string[] = [];
    const setStatus = (missionId: string, patch: Partial<{ loading: boolean; error: string | null; dirty: boolean; hintSeq: string | null }>) =>
      useMissionStore.setState((state) => ({
        sync: {
          ...state.sync,
          [missionId]: { atSeq: "1", revision: "1", loading: false, error: null, dirty: true, hintSeq: "2", ...patch },
        },
      }));
    // 실제 syncMission처럼 loading을 켰다가 끝에 결과를 남긴다(실패해도 dirty는 그대로).
    useMissionStore.setState({
      syncMission: async (missionId: string) => {
        calls.push(missionId);
        setStatus(missionId, { loading: true });
        await Promise.resolve();
        if (fail) setStatus(missionId, { error: "BUSY: daemon busy" });
        else setStatus(missionId, { dirty: false, hintSeq: null });
      },
    });
    const advance = async (ms: number) => {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(ms);
      });
    };
    let handle: ReturnType<typeof renderUi> | null = null;
    try {
      handle = renderUi(<MissionPage missionId={mission.id} />);
      await advance(0);
      expect(calls).toHaveLength(1);
      // 실패 직후 다시 뽑지 않는다(이전에는 loading이 풀릴 때마다 즉시 재시도했다).
      await advance(999);
      expect(calls).toHaveLength(1);
      await advance(1);
      expect(calls).toHaveLength(2);
      await advance(1999);
      expect(calls).toHaveLength(2);
      await advance(1);
      expect(calls).toHaveLength(3);
      await advance(3999);
      expect(calls).toHaveLength(3);
      await advance(1);
      expect(calls).toHaveLength(4);
      // 다음은 8s 뒤 — 이번에는 성공한다.
      fail = false;
      await advance(8000);
      expect(calls).toHaveLength(5);
      expect(useMissionStore.getState().sync[mission.id]).toMatchObject({ dirty: false, error: null });
      await advance(60_000);
      expect(calls).toHaveLength(5);
      // 새 힌트는 곧바로 뽑고, 다시 실패하면 간격이 처음(1s)부터 시작한다.
      fail = true;
      act(() => useMissionStore.getState().applyEventHint(mission.id, "5"));
      await advance(0);
      expect(calls).toHaveLength(6);
      await advance(999);
      expect(calls).toHaveLength(6);
      await advance(1);
      expect(calls).toHaveLength(7);
    } finally {
      handle?.unmount();
      vi.useRealTimers();
    }
  });
});

describe("빈 선택 자동 채우기(05 §5)", () => {
  it("선택이 없으면 실행 중인 Lead의 실행 내용을 본문에 바로 표시한다", () => {
    setViewportWidth(900);
    const mission = fakeMission();
    const builder = fakeTask(mission.id, "running", { title: "구현", ordinal: 1 });
    const lead = fakeTask(mission.id, "running", { title: "계획", kind: "plan", role: "lead", ordinal: 2 });
    seedStore({ missions: [mission], tasks: [builder, lead], runs: [fakeRun(mission.id, builder, "running"), fakeRun(mission.id, lead, "running")] });
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    expect(ui(mission.id)).toMatchObject({ selectedTaskId: lead.id, sheetOpen: false, selectedByUser: false });
    expect(query(handle.container, "side-sheet")).toBeNull();
    expect(query(handle.container, "run-detail")?.getAttribute("data-task-id")).toBe(lead.id);
    handle.unmount();
  });

  it("사용자가 고른 할 일은 유지한다", () => {
    const mission = fakeMission();
    const done = fakeTask(mission.id, "succeeded", { title: "조사" });
    const running = fakeTask(mission.id, "running", { title: "구현" });
    seedStore({ missions: [mission], tasks: [done, running], runs: [fakeRun(mission.id, running, "running")] });
    useMissionUiStore.getState().selectTask(mission.id, done.id);
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    expect(ui(mission.id).selectedTaskId).toBe(done.id);
    expect(query(handle.container, "run-detail")?.getAttribute("data-task-id")).toBe(done.id);
    click(query(handle.container, "toggle-lead")!);
    expect(query(handle.container, "run-detail")).toBeNull();
    click(handle.container.querySelector<HTMLButtonElement>(`button[data-task-id="${done.id}"]`)!);
    expect(query(handle.container, "run-detail")?.getAttribute("data-task-id")).toBe(done.id);

    handle.unmount();
  });

  it("중간 폭에서도 팀 탐색과 실행 본문을 나란히 표시한다", () => {
    setViewportWidth(900);
    const mission = fakeMission();
    seedStore({ missions: [mission] });
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    expect(handle.container.querySelector(".mission-workspace-team")).not.toBeNull();
    expect(handle.container.querySelector(".mission-workspace-content")).not.toBeNull();
    expect(handle.container.querySelector(".mission-detail-empty")).not.toBeNull();
    handle.unmount();
  });
});

describe("종료 확인 대기와 멈춘 실행(계약 C)", () => {
  it("멈추는 중에는 종료 증거 없는 불확실 실행도 대기 수에 넣고, 첫 실행 열기는 그 실행을 먼저 연다", () => {
    const mission = fakeMission({ state: "stopping" });
    const live = fakeTask(mission.id, "running", { title: "멈추는 실행" });
    const liveRun = fakeRun(mission.id, live, "stopping", { started_at: "2026-09-13T01:00:00.000Z" });
    const stuck = fakeTask(mission.id, "blocked", { title: "종료 불명 실행", blocked_code: "outcome_unknown" });
    const stuckRun = fakeRun(mission.id, stuck, "unknown", { started_at: "2026-09-13T02:00:00.000Z" });
    const ended = fakeTask(mission.id, "blocked", { title: "종료 확인된 실행", blocked_code: "outcome_unknown_ended" });
    const endedRun = fakeRun(mission.id, ended, "interrupted");
    endedRun.reconciliation_ref = endedRun.context_ref;
    seedStore({ missions: [mission], tasks: [live, stuck, ended], runs: [liveRun, stuckRun, endedRun] });
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    click(query(handle.container, "mission-control")!);
    const menu = query(handle.container, "mission-control-menu")!;
    expect(query(menu, "control-waiting")?.textContent).toBe(t("missions.control.waitingRuns", { count: 2 }));
    click(query(menu, "control-open-first-run")!);
    expect(ui(mission.id)).toMatchObject({ selectedTaskId: stuck.id, rightPane: "detail" });
    handle.unmount();
  });
});

describe("초안 시작·삭제", () => {
  it("시작은 최신 revision으로 start를 보내고, 기준 커밋이 바뀌어 거절되면 같은 목표로 새 작업을 연다", async () => {
    const client = installMockClient();
    const mission = fakeMission({ state: "draft", phase: "planning", revision: "3" });
    seedStore({ missions: [mission] });
    openTab(mission);
    fakeSync();
    const control = vi
      .spyOn(client, "missionControl")
      .mockResolvedValueOnce({ mission_id: mission.id, revision: "4", event_seq: "1", entity_ids: [] })
      .mockRejectedValueOnce(new RpcClientError("INVALID_STATE", "base moved", false, { reason_code: "base_changed" }));
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    await flushAsync(3);
    expect(query(handle.container, "mission-start")?.textContent).toBe(t("missions.draft.start"));

    click(query(handle.container, "mission-start")!);
    await flushAsync(4);
    expect(control.mock.calls[0][0]).toMatchObject({ mission_id: mission.id, expected_revision: "3", action: "start" });
    expect(query(handle.container, "mission-control-error")).toBeNull();

    click(query(handle.container, "mission-start")!);
    await flushAsync(4);
    const error = query(handle.container, "mission-control-error")!;
    expect(query(error, "mission-error-message")?.textContent).toBe(t("missions.error.reason.baseChanged"));
    const recreate = query(error, "mission-start-recreate")!;
    expect(recreate.textContent).toBe(t("missions.outcome.sameGoal"));
    click(recreate);
    await flushAsync(4);
    expect(useWorkbenchStore.getState().missionCreate).toMatchObject({ kind: "mission-create", repositoryPath: mission.repository_path });
    handle.unmount();
  });

  it("시작 전 작업이 아니면 시작·초안 삭제 버튼이 없다", () => {
    const mission = fakeMission({ state: "running" });
    seedStore({ missions: [mission] });
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    expect(query(handle.container, "mission-start")).toBeNull();
    expect(query(handle.container, "mission-draft-delete")).toBeNull();
    handle.unmount();
  });

  it("초안 삭제는 확인 뒤 중단하고 보관해 탭을 닫고 되돌리기 토스트를 띄운다", async () => {
    const client = installMockClient();
    const mission = fakeMission({ state: "draft", phase: "planning", revision: "3" });
    seedStore({ missions: [mission] });
    openTab(mission);
    let daemon: Mission = mission;
    fakeSync(() => seedStore({ missions: [daemon] }));
    const control = vi.spyOn(client, "missionControl").mockImplementation(async (params) => {
      daemon = params.action === "cancel"
        ? { ...daemon, state: "cancelled", revision: "4" }
        : { ...daemon, archived_at: "2026-09-13T03:00:00.000Z", revision: "5" };
      return { mission_id: mission.id, revision: daemon.revision, event_seq: "1", entity_ids: [] };
    });
    const handle = renderUi(<MissionPage missionId={mission.id} />);
    await flushAsync(3);
    click(query(handle.container, "mission-draft-delete")!);
    const confirm = query(handle.container, "mission-draft-delete-confirm")!;
    expect(confirm.getAttribute("role")).toBe("alertdialog");
    expect(confirm.textContent).toContain(t("missions.draft.deleteBody"));
    expect(control).not.toHaveBeenCalled();
    click(query(confirm, "mission-draft-delete-ok")!);
    await flushAsync(8);
    expect(control.mock.calls.map((call) => call[0].action)).toEqual(["cancel", "archive"]);
    expect(control.mock.calls[0][0]).toMatchObject({ expected_revision: "3" });
    expect(control.mock.calls[1][0]).toMatchObject({ expected_revision: "4" });
    expect(useWorkbenchStore.getState().tabs.some((tab) => tab.id === TAB_ID)).toBe(false);
    expect(query(document.body, "archive-undo-toast")).not.toBeNull();
    handle.unmount();
  });
});

describe("보관 뒤 작업 공간 정리 안내(계약 D)", () => {
  it("정리할 수 있으면 되돌리기 토스트에 짧은 안내와 목록 열기를 덧붙이고, 조회 실패면 붙이지 않는다", async () => {
    const client = installMockClient();
    const mission = fakeMission({ state: "completed", phase: "done" });
    seedStore({ missions: [mission] });
    openTab(mission);
    fakeSync();
    vi.spyOn(client, "missionControl").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "1", entity_ids: [] });
    const usage = vi.fn(async (_params: { mission_id: string | null }) => ({
      missions: [{ mission_id: mission.id, workspaces: 2, bytes: String(3 * 1024 * 1024), cleanable: true, blocked_reason: null }],
      total_bytes: String(3 * 1024 * 1024),
    }));
    Object.assign(client, { workspaceUsage: usage });
    let handle = renderUi(<MissionPage missionId={mission.id} />);
    click(query(handle.container, "mission-archive")!);
    click(query(query(handle.container, "mission-archive-confirm")!, "mission-archive-ok")!);
    await flushAsync(6);
    expect(usage).toHaveBeenCalledWith({ mission_id: mission.id });
    const hint = query(document.body, "archive-cleanup-hint")!;
    expect(hint.textContent).toContain(t("missions.workspaceCleanup.archivedHint", { size: "3 MB" }));
    click(query(hint, "archive-cleanup-open-list")!);
    expect(useWorkbenchStore.getState().modal).toMatchObject({ kind: "mission-list" });
    handle.unmount();
    dismissArchiveUndoToast();
    await flushAsync(1);

    Object.assign(client, { workspaceUsage: vi.fn(async () => { throw new RpcClientError("INTERNAL", "no usage"); }) });
    seedStore({ missions: [{ ...mission, archived_at: null }] });
    openTab(mission);
    handle = renderUi(<MissionPage missionId={mission.id} />);
    click(query(handle.container, "mission-archive")!);
    click(query(query(handle.container, "mission-archive-confirm")!, "mission-archive-ok")!);
    await flushAsync(6);
    expect(query(document.body, "archive-undo-toast")).not.toBeNull();
    expect(query(document.body, "archive-cleanup-hint")).toBeNull();
    handle.unmount();
  });
});
