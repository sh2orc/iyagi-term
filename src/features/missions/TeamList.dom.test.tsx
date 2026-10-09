/**
 * TeamList 시험(05 §5): 상태 렌더링 · 카운터 · 필터 · 검색 · 더보기.
 * happy-dom( *.dom.test.tsx 환경)에서 상호작용까지 확인한다.
 */

import { act } from "react";
import { beforeEach, describe, expect, it } from "vitest";
import { TeamList, type TeamListProps } from "./TeamList";
import {
  click,
  compatibleBinding,
  fakeDecision,
  fakeMission,
  fakeRun,
  fakeTask,
  renderUi,
  resetAllMissionState,
  seedStore,
} from "./testSupport";
import { useMissionStore } from "./store";
import { blockedReasonLabel, runStateLabel } from "./labels";
import { t } from "../../i18n";

const MISSION_ID = "mission-teamlist";

function seedTeam() {
  const mission = fakeMission({ id: MISSION_ID });
  const running = fakeTask(MISSION_ID, "running", { title: "API 구현" });
  const runningRun = fakeRun(MISSION_ID, running, "running", { requested_model: "GLM-5.3" });
  const starting = fakeTask(MISSION_ID, "running", { title: "UI 구현" });
  const startingRun = fakeRun(MISSION_ID, starting, "starting", { requested_model: "Claude-Sonnet-4" });
  const awaiting = fakeTask(MISSION_ID, "awaiting_input", { title: "통합 검증" });
  const planned = fakeTask(MISSION_ID, "planned", { title: "문서화", depends_on: [running.id] });
  const succeeded = fakeTask(MISSION_ID, "succeeded", { title: "조사" });
  const failed = fakeTask(MISSION_ID, "failed", { title: "배포", attempt_count: 2 });
  const decision = fakeDecision(MISSION_ID);
  seedStore({
    missions: [mission],
    tasks: [running, starting, awaiting, planned, succeeded, failed],
    runs: [runningRun, startingRun],
    decisions: [decision],
  });
  return { running, starting, awaiting, planned, succeeded, failed, decision };
}

beforeEach(() => {
  resetAllMissionState();
});

describe("TeamList 상태 렌더링", () => {
  it("05 §5 표의 상태 문구를 아이콘+텍스트로 표시한다", () => {
    seedTeam();
    const handle = renderUi(
      <TeamList
        missionId={MISSION_ID}
        selectedTaskId={null}
        filter="all"
        search=""
        onSelectTask={() => undefined}
        onFilterChange={() => undefined}
        onSearchChange={() => undefined}
      />,
    );
    const text = handle.container.textContent ?? "";
    expect(text).toContain(t("missions.taskStatus.running"));
    expect(text).toContain(t("missions.taskStatus.starting"));
    expect(text).toContain(t("missions.taskStatus.awaitingInput"));
    expect(text).toContain(t("missions.taskStatus.planned"));
    expect(text).toContain(t("missions.taskStatus.succeeded"));
    expect(text).toContain(t("missions.taskStatus.failed"));
    // 색 외 수단: 상태 글리프가 상태 클래스와 함께 렌더된다.
    const glyph = handle.container.querySelector(".mission-task-status.status-running");
    expect(glyph?.textContent).toBe("▶");
    handle.unmount();
  });

  it("카운터는 실행 N(live Run) · 완료 N(succeeded Task) · 결정 N(open Decision)", () => {
    seedTeam();
    // live Run은 starting 1 + running 1 = 2.
    const handle = renderUi(
      <TeamList
        missionId={MISSION_ID}
        selectedTaskId={null}
        filter="all"
        search=""
        onSelectTask={() => undefined}
        onFilterChange={() => undefined}
        onSearchChange={() => undefined}
      />,
    );
    const counters = handle.container.querySelector("[data-testid='team-counters']");
    expect(counters?.textContent).toContain(t("missions.team.liveCount", { count: 2 }));
    expect(counters?.textContent).toContain(t("missions.team.doneCount", { count: 1 }));
    expect(counters?.textContent).toContain(t("missions.team.decisionCount", { count: 1 }));
    handle.unmount();
  });
});

describe("TeamList 필터와 검색", () => {
  it("실행 중 필터는 running task만 남긴다", () => {
    seedTeam();
    const handle = renderUi(
      <TeamList
        missionId={MISSION_ID}
        selectedTaskId={null}
        filter="running"
        search=""
        onSelectTask={() => undefined}
        onFilterChange={() => undefined}
        onSearchChange={() => undefined}
      />,
    );
    const rows = handle.container.querySelectorAll("[data-testid='team-row']");
    expect(rows.length).toBe(2);
    expect(handle.container.textContent).not.toContain(t("missions.taskStatus.planned"));
    handle.unmount();
  });

  it("검색은 제목·모델로 좁힌다", () => {
    seedTeam();
    const handle = renderUi(
      <TeamList
        missionId={MISSION_ID}
        selectedTaskId={null}
        filter="all"
        search="GLM-5.3"
        onSelectTask={() => undefined}
        onFilterChange={() => undefined}
        onSearchChange={() => undefined}
      />,
    );
    const rows = handle.container.querySelectorAll("[data-testid='team-row']");
    expect(rows.length).toBe(1);
    expect(rows[0].textContent).toContain("API 구현");
    handle.unmount();
  });
});

describe("TeamList 상한과 더보기(05 §5: 12+ 행)", () => {
  it("12행까지만 그리고 더보기로 노출한다", () => {
    const mission = fakeMission({ id: MISSION_ID });
    const tasks = Array.from({ length: 15 }, () => fakeTask(MISSION_ID, "planned"));
    seedStore({ missions: [mission], tasks });
    const handle = renderUi(
      <TeamList
        missionId={MISSION_ID}
        selectedTaskId={null}
        filter="all"
        search=""
        onSelectTask={() => undefined}
        onFilterChange={() => undefined}
        onSearchChange={() => undefined}
      />,
    );
    expect(handle.container.querySelectorAll("[data-testid='team-row']").length).toBe(12);
    click(handle.container.querySelector("[data-testid='team-more']") as Element);
    expect(handle.container.querySelectorAll("[data-testid='team-row']").length).toBe(15);
    handle.unmount();
  });
});

describe("TeamList 정렬 안정성(05 §5: 재정렬 금지)", () => {
  it("상태가 바뀌어도 plan ordinal 순서를 유지한다", () => {
    const mission = fakeMission({ id: MISSION_ID });
    const first = fakeTask(MISSION_ID, "planned", { ordinal: 1, title: "첫째" });
    const second = fakeTask(MISSION_ID, "planned", { ordinal: 2, title: "둘째" });
    seedStore({ missions: [mission], tasks: [first, second] });
    const handle = renderUi(
      <TeamList
        missionId={MISSION_ID}
        selectedTaskId={null}
        filter="all"
        search=""
        onSelectTask={() => undefined}
        onFilterChange={() => undefined}
        onSearchChange={() => undefined}
      />,
    );
    const titles = () =>
      [...handle.container.querySelectorAll(".mission-task-title")].map((node) => node.textContent);
    expect(titles()).toEqual(["첫째", "둘째"]);
    // 상태만 바뀌면(성공) — 순서는 그대로.
    useMissionStore.setState((state) => ({
      tasks: { ...state.tasks, [second.id]: { ...state.tasks[second.id], state: "succeeded" } },
    }));
    expect(titles()).toEqual(["첫째", "둘째"]);
    handle.unmount();
  });
});

describe("TeamList 키보드 선택", () => {
  it("ArrowDown으로 선택이 이동한다", () => {
    const seeded = seedTeam();
    let selected: string | null = null;
    const handle = renderUi(
      <TeamList
        missionId={MISSION_ID}
        selectedTaskId={null}
        filter="all"
        search=""
        onSelectTask={(taskId) => {
          selected = taskId;
        }}
        onFilterChange={() => undefined}
        onSearchChange={() => undefined}
      />,
    );
    const list = handle.container.querySelector("[data-testid='team-list']") as HTMLElement;
    list.dispatchEvent(
      new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true, cancelable: true }),
    );
    expect(selected).toBe(seeded.running.id);
    handle.unmount();
  });
});

function renderTeam(overrides: Partial<TeamListProps> = {}) {
  return renderUi(
    <TeamList
      missionId={MISSION_ID}
      selectedTaskId={null}
      filter="all"
      search=""
      onSelectTask={() => undefined}
      onFilterChange={() => undefined}
      onSearchChange={() => undefined}
      {...overrides}
    />,
  );
}

function extraOf(container: HTMLElement, title: string): HTMLElement {
  const row = [...container.querySelectorAll<HTMLElement>("[data-testid='team-row']")].find(
    (node) => node.querySelector(".mission-task-title")?.textContent === title,
  );
  return row?.parentElement?.querySelector<HTMLElement>("[data-testid='team-row-extra']") as HTMLElement;
}

const minutesAgo = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

describe("TeamList 응답 필요 필터(열린 결정 기준)", () => {
  it("열린 결정이 영향을 주는 할 일만 남긴다", () => {
    const mission = fakeMission({ id: MISSION_ID });
    const waitingInput = fakeTask(MISSION_ID, "awaiting_input", { title: "입력 대기" });
    const costBlocked = fakeTask(MISSION_ID, "blocked", { title: "비용 대기", blocked_code: "cost_limit" });
    const answered = fakeTask(MISSION_ID, "blocked", { title: "이미 답함" });
    seedStore({
      missions: [mission],
      tasks: [waitingInput, costBlocked, answered],
      decisions: [
        fakeDecision(MISSION_ID, { affected_task_ids: [costBlocked.id] }),
        fakeDecision(MISSION_ID, { state: "answered", affected_task_ids: [answered.id] }),
      ],
    });
    const handle = renderTeam({ filter: "awaiting" });
    const titles = [...handle.container.querySelectorAll(".mission-task-title")].map((node) => node.textContent);
    expect(titles).toEqual(["비용 대기"]);
    handle.unmount();
  });
});

describe("TeamList 보조 정보(05 §5)", () => {
  it("남은 재시도는 작업 정책의 할 일당 시도 한도로 센다", () => {
    const mission = fakeMission({ id: MISSION_ID });
    mission.policy = { ...mission.policy, max_attempts_per_task: 5 };
    const failed = fakeTask(MISSION_ID, "failed", { title: "배포", attempt_count: 2 });
    seedStore({ missions: [mission], tasks: [failed] });
    const handle = renderTeam();
    expect(handle.container.querySelector("[data-testid='retry-left']")?.textContent).toBe(
      t("missions.team.retryLeft", { count: 3 }),
    );
    handle.unmount();
  });

  it("선행 작업 실패는 이유와 실패한 선행 할 일 링크를 보이고, 링크는 그 할 일을 선택한다", () => {
    const mission = fakeMission({ id: MISSION_ID });
    const upstream = fakeTask(MISSION_ID, "failed", { title: "스키마 변경" });
    const ok = fakeTask(MISSION_ID, "succeeded", { title: "조사" });
    const blocked = fakeTask(MISSION_ID, "blocked", {
      title: "API 연결",
      blocked_code: "dependency_failed",
      depends_on: [ok.id, upstream.id],
    });
    const unknown = fakeTask(MISSION_ID, "blocked", { title: "기타", blocked_code: "recovery_held" });
    seedStore({ missions: [mission], tasks: [upstream, ok, blocked, unknown] });
    let selected: string | null = null;
    const handle = renderTeam({ onSelectTask: (taskId) => { selected = taskId; } });
    const reason = extraOf(handle.container, "API 연결").querySelector("[data-testid='blocked-reason']");
    expect(reason?.textContent).toContain(blockedReasonLabel(t, "dependency_failed"));
    const links = reason?.querySelectorAll("[data-testid='dependency-link']") ?? [];
    expect([...links].map((link) => link.textContent)).toEqual(["스키마 변경"]);
    click(links[0]);
    expect(selected).toBe(upstream.id);
    expect(extraOf(handle.container, "기타").textContent).toContain(blockedReasonLabel(t, "recovery_held"));
    expect(extraOf(handle.container, "기타").textContent).not.toContain("recovery_held");
    handle.unmount();
  });

  it("활동이 3분 이상 없을 때만 상태 확인 중, 그 전에는 실행 중 경과를 보인다", () => {
    const mission = fakeMission({ id: MISSION_ID });
    const fresh = fakeTask(MISSION_ID, "running", { title: "활발" });
    const freshRun = fakeRun(MISSION_ID, fresh, "running", { started_at: minutesAgo(5), last_activity_at: minutesAgo(1) });
    const quiet = fakeTask(MISSION_ID, "running", { title: "조용" });
    const quietRun = fakeRun(MISSION_ID, quiet, "running", { started_at: minutesAgo(10), last_activity_at: minutesAgo(4) });
    seedStore({ missions: [mission], tasks: [fresh, quiet], runs: [freshRun, quietRun] });
    const handle = renderTeam();
    const freshText = extraOf(handle.container, "활발").querySelector("[data-testid='running-activity']");
    expect(freshText?.getAttribute("data-stale")).toBe("false");
    expect(freshText?.textContent).toBe(
      t("missions.team.runningFor", { elapsed: t("missions.team.elapsedMinutes", { minutes: 5 }) }),
    );
    const quietText = extraOf(handle.container, "조용").querySelector("[data-testid='running-activity']");
    expect(quietText?.getAttribute("data-stale")).toBe("true");
    expect(quietText?.textContent).toBe(t("missions.team.staleActivity", { minutes: 4 }));
    handle.unmount();
  });

  it("작업이 일시정지면 대기 이유가 계속 실행을 기다린다고 말한다", () => {
    const mission = fakeMission({ id: MISSION_ID, state: "paused" });
    const ready = fakeTask(MISSION_ID, "ready", { title: "대기 중" });
    seedStore({ missions: [mission], tasks: [ready] });
    const handle = renderTeam();
    expect(extraOf(handle.container, "대기 중").querySelector("[data-testid='ready-reason']")?.textContent).toBe(
      t("missions.team.pausedReady"),
    );
    act(() => {
      useMissionStore.setState((state) => ({
        missions: { ...state.missions, [MISSION_ID]: { ...state.missions[MISSION_ID], state: "running" } },
      }));
    });
    const reason = extraOf(handle.container, "대기 중").querySelector("[data-testid='ready-reason']")?.textContent ?? "";
    expect(reason).toBe(t("missions.team.readyReason"));
    expect(reason.toLowerCase()).not.toContain("slot");
    handle.unmount();
  });

  it("시도 이력의 실행 상태는 원문 대신 사람 문구로 보인다", () => {
    const mission = fakeMission({ id: MISSION_ID });
    const task = fakeTask(MISSION_ID, "running", { title: "재시도", attempt_count: 2 });
    const first = fakeRun(MISSION_ID, task, "interrupted", { attempt: 1 });
    const second = fakeRun(MISSION_ID, task, "running", { attempt: 2 });
    seedStore({ missions: [mission], tasks: [task], runs: [first, second] });
    const handle = renderTeam();
    click(handle.container.querySelector("[data-testid='attempt-toggle']") as Element);
    const history = handle.container.querySelector("[data-testid='attempt-list']")?.textContent ?? "";
    expect(history).toContain(runStateLabel(t, "interrupted"));
    expect(history).not.toContain("interrupted");
    handle.unmount();
  });
});

describe("TeamList 실험적 연결(계약 A)", () => {
  it("실험적 연결로 실행한 시도가 있는 행에만 작은 '실험적' 배지와 툴팁을 보인다", () => {
    const mission = fakeMission({ id: MISSION_ID });
    const experimental = compatibleBinding();
    experimental.capabilities.cancel = { supported: true, reason_code: "experimental_opt_in" };
    const flagged = fakeTask(MISSION_ID, "running", { title: "실험 연결 작업", attempt_count: 2 });
    const firstAttempt = fakeRun(MISSION_ID, flagged, "failed", { attempt: 1, binding_snapshot: experimental });
    const secondAttempt = fakeRun(MISSION_ID, flagged, "running", { attempt: 2, binding_snapshot: compatibleBinding() });
    const plain = fakeTask(MISSION_ID, "running", { title: "일반 작업" });
    const plainRun = fakeRun(MISSION_ID, plain, "running", { binding_snapshot: compatibleBinding() });
    seedStore({ missions: [mission], tasks: [flagged, plain], runs: [firstAttempt, secondAttempt, plainRun] });
    const handle = renderUi(
      <TeamList
        missionId={MISSION_ID}
        selectedTaskId={null}
        filter="all"
        search=""
        onSelectTask={() => undefined}
        onFilterChange={() => undefined}
        onSearchChange={() => undefined}
      />,
    );
    const row = (taskId: string) => handle.container.querySelector<HTMLElement>(`[data-task-id="${taskId}"]`)!;
    const badge = row(flagged.id).querySelector("[data-testid=team-row-experimental]")!;
    expect(badge.textContent).toContain(t("missions.experimental.badge"));
    expect(badge.getAttribute("title")).toBe(t("missions.experimentalRun.tooltip"));
    expect(row(plain.id).querySelector("[data-testid=team-row-experimental]")).toBeNull();
    handle.unmount();
  });
});
