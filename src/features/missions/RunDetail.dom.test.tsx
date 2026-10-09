/**
 * RunDetail(05 §6) 사용성 계약: 사람 문구 라벨, 실행 탭 노출 조건, 전체 결과
 * 변경/검증 탭, 활동 로딩/빈 상태/따라가기, 비활성 사유, 대기 이유·보고,
 * mutateWithResync + MissionErrorNotice.
 */

import { act } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ExecRecord } from "../../generated/ExecRecord";
import type { MissionActivityResult } from "../../generated/MissionActivityResult";
import type { Mission } from "../../generated/Mission";
import type { MissionRunAttestExitedParams } from "../../generated/MissionRunAttestExitedParams";
import type { MutationResult } from "../../generated/MutationResult";
import type { Run } from "../../generated/Run";
import { RpcClientError } from "../daemon/client";
import { t } from "../../i18n";
import { newBinding } from "./configuration";
import { RunDetail } from "./RunDetail";
import { useMissionStore } from "./store";
import {
  click,
  compatibleBinding,
  dispatch,
  fakeCandidate,
  fakeMission,
  fakeRun,
  fakeTask,
  fakeVerification,
  flushAsync,
  installMockClient,
  renderUi,
  resetAllMissionState,
  seedStore,
  uploadMockText,
} from "./testSupport";

beforeEach(resetAllMissionState);

const result = (mission: Mission) => ({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [] });
const noop = () => undefined;

describe("RunDetail 헤더", () => {
  it("역할·실행 상태를 사람 문구로 보여 주고 원문 상태·실행 id와 낭독 영역을 노출하지 않는다", () => {
    const mission = fakeMission();
    const task = fakeTask(mission.id, "running", { role: "test_author", attempt_count: 2 });
    const run = fakeRun(mission.id, task, "awaiting_input");
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=detail-role]")?.textContent).toBe(t("missions.role.testAuthor"));
    expect(ui.container.querySelector("[data-testid=detail-run-state]")?.textContent).toBe(t("missions.runState.awaitingInput"));
    expect(ui.container.textContent).toContain(t("missions.detail.attemptOf", { n: 2, limit: mission.policy.max_attempts_per_task }));
    expect(ui.container.textContent).not.toContain(run.id);
    expect(ui.container.textContent).not.toContain("awaiting_input");
    expect(ui.container.textContent).not.toContain("test_author");
    const note = ui.container.querySelector("[data-testid=detail-saved-note]")!;
    expect(note.getAttribute("title")).toBe(t("missions.time.savedNote"));
    expect(note.getAttribute("aria-label")).toContain(t("missions.time.savedNote"));
    expect(ui.container.querySelector("[role=status], [aria-live]")).toBeNull();
    ui.unmount();
  });

  it("대기 중인 할 일은 이유와 담당 AI 보고 본문을 보여 주고, 전용 안내가 있으면 이유를 겹치지 않는다", async () => {
    const client = installMockClient();
    const report = await uploadMockText(client, "API 키가 없어 배포 단계를 진행할 수 없습니다.");
    const agentResult = await uploadMockText(client, JSON.stringify({ kind: "blocked", code: "needs_credentials", report_ref: report }));
    const mission = fakeMission();
    const task = fakeTask(mission.id, "blocked", { blocked_code: "provider_blocked:needs_credentials" });
    const run = fakeRun(mission.id, task, "succeeded", { result_ref: agentResult, ended_at: "2026-09-13T02:00:00.000Z" });
    task.active_run_id = null;
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    let ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    await flushAsync();
    expect(ui.container.querySelector("[data-testid=blocked-reason]")?.textContent)
      .toContain(t("missions.detail.blockedReason", { reason: t("missions.blocked.providerBlocked") }));
    expect(ui.container.querySelector("[data-testid=blocked-report]")?.textContent).toBe("API 키가 없어 배포 단계를 진행할 수 없습니다.");
    ui.unmount();

    const waiting = fakeTask(mission.id, "blocked", { blocked_code: "dependency_failed" });
    seedStore({ tasks: [waiting] });
    ui = renderUi(<RunDetail mission={mission} task={waiting} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=blocked-reason]")?.textContent)
      .toBe(t("missions.detail.blockedReason", { reason: t("missions.blocked.dependencyFailed") }));
    expect(ui.container.querySelector("[data-testid=blocked-report]")).toBeNull();
    ui.unmount();

    const limited = fakeTask(mission.id, "blocked", { blocked_code: "provider_rate_limited", dispatch_after_unix_ms: "1" });
    seedStore({ tasks: [limited] });
    ui = renderUi(<RunDetail mission={mission} task={limited} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=blocked-reason]")).toBeNull();
    expect(ui.container.querySelector("[data-testid=rate-limit-notice]")).not.toBeNull();
    expect(ui.container.querySelector("[role=status], [aria-live]")).toBeNull();
    ui.unmount();
  });

  it("상태 안내의 모델 변경 행동은 재배정 선택기를 연다", async () => {
    const client = installMockClient();
    const binding = { ...compatibleBinding(), label: "Alt", model_id: "alt-model" };
    const mission = fakeMission();
    mission.policy.allowed_binding_ids = [binding.id];
    client.bindingList = vi.fn(async () => ({ bindings: [binding] }));
    const task = fakeTask(mission.id, "blocked", { blocked_code: "provider_rate_limited", dispatch_after_unix_ms: "1" });
    seedStore({ missions: [mission], tasks: [task] });
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=reassign-picker]")).toBeNull();
    click(ui.container.querySelector("[data-testid=rate-limit-change-model]")!);
    await flushAsync();
    const pick = ui.container.querySelector<HTMLButtonElement>("[data-testid=reassign-picker] button")!;
    expect(pick.textContent).toContain("Alt");
    expect(pick.title).toBe(t("missions.detail.reassignOnlyTitle", { model: "alt-model" }));
    ui.unmount();
  });
});

describe("RunDetail 탭", () => {
  it("검증된 native attach가 없으면 실행 탭을 숨기고 방향키 탐색도 건너뛴다", () => {
    const mission = fakeMission();
    const task = fakeTask(mission.id, "running");
    const run = fakeRun(mission.id, task, "running", { binding_snapshot: newBinding() });
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    const onTab = vi.fn();
    let ui = renderUi(<RunDetail mission={mission} task={task} detailTab="exec" onDetailTabChange={onTab} />);
    expect(ui.container.querySelector("[data-testid=detail-tab-exec]")).toBeNull();
    expect(ui.container.querySelector("[data-testid=detail-tab-activity]")?.getAttribute("aria-selected")).toBe("true");
    expect(ui.container.querySelector("#mission-detail-panel-activity")).not.toBeNull();
    dispatch(ui.container.querySelector("[role=tablist]")!, new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }));
    expect(onTab).toHaveBeenCalledWith("changes");
    ui.unmount();

    seedStore({ runs: [{ ...run, binding_snapshot: compatibleBinding() }] });
    ui = renderUi(<RunDetail mission={mission} task={task} detailTab="exec" onDetailTabChange={onTab} />);
    expect(ui.container.querySelector("[data-testid=detail-tab-exec]")?.getAttribute("aria-selected")).toBe("true");
    ui.unmount();
  });

  it("변경 탭은 전체 결과임을 밝히고 manifest entries 중 이 할 일 범위의 파일을 먼저 보여 준다", async () => {
    const client = installMockClient();
    const manifest = {
      base_oid: "b".repeat(40),
      entries: [
        { path: "README.md", change: "modified", bytes: 10, sha256: "x" },
        { path: "src/api/users.ts", change: "added", bytes: 20, sha256: "y" },
        { path: "src/api/old.ts", change: "deleted", bytes: 0, sha256: "" },
      ],
    };
    const ref = await uploadMockText(client, JSON.stringify(manifest));
    const mission = fakeMission({ phase: "validating" });
    const task = fakeTask(mission.id, "succeeded");
    task.contract.allowed_paths = ["src/api/**"];
    const run = fakeRun(mission.id, task, "succeeded");
    task.active_run_id = null;
    const candidate = fakeCandidate(mission.id, { manifest_ref: ref, source_run_ids: [run.id] });
    mission.candidate_id = candidate.id;
    seedStore({ missions: [mission], tasks: [task], runs: [run], candidates: [candidate] });
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    await flushAsync();
    expect(ui.container.querySelector("[data-testid=detail-tab-changes]")?.textContent).toBe(t("missions.detail.tab.changesAll"));
    expect(ui.container.querySelector("[data-testid=detail-tab-verification]")?.textContent).toBe(t("missions.detail.tab.verification"));
    expect(ui.container.querySelector("[data-testid=changes-state]")?.textContent).toBe(t("missions.detail.changesCaptured", { count: 3 }));
    expect(ui.container.querySelector("[data-testid=changes-task-inclusion]")?.textContent).toBe(t("missions.detail.changes.taskIncluded"));
    const entries = (selector: string) => [...ui.container.querySelectorAll(`${selector} [data-testid=changes-entry]`)].map((entry) => entry.textContent);
    expect(entries("[data-testid=changes-task-scope]")).toEqual(["+ src/api/users.ts", "- src/api/old.ts"]);
    expect(entries("[data-testid=changes-other]")).toEqual(["~ README.md"]);
    const scope = ui.container.querySelector("[data-testid=changes-task-scope]")!;
    const other = ui.container.querySelector("[data-testid=changes-other]")!;
    expect(scope.compareDocumentPosition(other) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    ui.unmount();
  });

  it("검증 탭은 실패와 입력 무결성 문구를 섞지 않고 이 할 일 관련 검증을 먼저 보여 준다", () => {
    const mission = fakeMission({ phase: "validating" });
    const task = fakeTask(mission.id, "succeeded");
    task.contract.requirement_ids = ["req-2"];
    const candidate = fakeCandidate(mission.id);
    mission.candidate_id = candidate.id;
    const unrelated = fakeVerification(mission.id, candidate.id, { requirement_ids: ["req-1"], status: "passed", input_integrity: "enforced",
      started_at: "2026-09-13T01:00:00.000Z" });
    const related = fakeVerification(mission.id, candidate.id, { requirement_ids: ["req-2"], status: "failed", input_integrity: "observed",
      exit_code: 1, started_at: "2026-09-13T02:00:00.000Z" });
    seedStore({ missions: [mission], tasks: [task], candidates: [candidate], verifications: [unrelated, related] });
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="verification" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=detail-tab-verification]")?.textContent).toBe(t("missions.detail.tab.verificationAll"));
    const rows = [...ui.container.querySelectorAll("[data-testid=verification-row]")];
    expect(rows).toHaveLength(2);
    expect(ui.container.querySelector("[data-testid=verification-task]")?.contains(rows[0])).toBe(true);
    const failed = rows[0].textContent ?? "";
    expect(failed).toContain(t("missions.verification.failed"));
    expect(failed).toContain(t("missions.integrity.note.observed"));
    expect(failed).toContain(t("missions.detail.verification.exit", { code: 1 }));
    expect(failed).not.toContain(t("missions.verification.passed"));
    expect(rows[1].textContent).toContain(t("missions.verification.passed"));
    expect(rows[1].textContent).toContain(t("missions.integrity.note.enforced"));
    ui.unmount();
  });
});

describe("RunDetail 활동", () => {
  it("불러오는 중과 활동 없음, 실행 전 상태를 구분한다", async () => {
    const client = installMockClient();
    const mission = fakeMission();
    const task = fakeTask(mission.id, "running");
    const run = fakeRun(mission.id, task, "running");
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    let resolve!: (page: MissionActivityResult) => void;
    client.missionActivity = vi.fn(() => new Promise<MissionActivityResult>((done) => { resolve = done; }));
    let ui = renderUi(<RunDetail mission={mission} task={task} detailTab="activity" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=activity-loading]")?.textContent).toBe(t("missions.detail.activityLoading"));
    await act(async () => {
      resolve({ body_ref: null, next_offset: "0", complete: true });
    });
    await flushAsync();
    expect(ui.container.querySelector("[data-testid=activity-loading]")).toBeNull();
    expect(ui.container.querySelector("[data-testid=activity-empty]")?.textContent).toBe(t("missions.detail.activityEmpty"));
    ui.unmount();

    const idle = fakeTask(mission.id, "ready");
    seedStore({ tasks: [idle] });
    ui = renderUi(<RunDetail mission={mission} task={idle} detailTab="activity" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=activity-empty]")?.textContent).toBe(t("missions.detail.activityNoRun"));
    ui.unmount();
  });

  it("새 출력을 따라가다 사용자가 위로 스크롤하면 멈추고 최신으로 버튼을 보여 준다", async () => {
    const client = installMockClient();
    const body = await uploadMockText(client, "line 1\nline 2\n");
    const mission = fakeMission();
    const task = fakeTask(mission.id, "running");
    const run = fakeRun(mission.id, task, "running");
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    client.missionActivity = vi.fn(async () => ({ body_ref: body, next_offset: body.bytes, complete: true }));
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="activity" onDetailTabChange={noop} />);
    await flushAsync();
    const log = ui.container.querySelector<HTMLPreElement>("[data-testid=activity-log]")!;
    expect(log.textContent).toContain("line 2");
    expect(ui.container.querySelector("[data-testid=activity-latest]")).toBeNull();
    Object.defineProperty(log, "scrollHeight", { configurable: true, value: 1000 });
    Object.defineProperty(log, "clientHeight", { configurable: true, value: 200 });
    Object.defineProperty(log, "scrollTop", { configurable: true, writable: true, value: 100 });
    dispatch(log, new Event("scroll"));
    const latest = ui.container.querySelector("[data-testid=activity-latest]")!;
    expect(latest.textContent).toBe(t("missions.detail.activityLatest"));
    click(latest);
    expect(log.scrollTop).toBe(1000);
    expect(ui.container.querySelector("[data-testid=activity-latest]")).toBeNull();
    ui.unmount();
  });
});

describe("RunDetail 제어", () => {
  it("시작 전 할 일은 할 일 취소로, 의존 작업이 없으면 개수 문구 없이 확인한다", () => {
    const mission = fakeMission();
    const ready = fakeTask(mission.id, "ready", { required: false });
    seedStore({ missions: [mission], tasks: [ready] });
    let ui = renderUi(<RunDetail mission={mission} task={ready} detailTab="changes" onDetailTabChange={noop} />);
    const cancel = ui.container.querySelector("[data-testid=task-cancel]")!;
    expect(cancel.textContent).toBe(t("missions.detail.actionCancelTask"));
    click(cancel);
    const confirm = ui.container.querySelector("[data-testid=task-cancel-confirm]")!.textContent ?? "";
    expect(confirm).toContain(t("missions.detail.actionCancelConfirmPlain"));
    expect(confirm).not.toContain(t("missions.detail.actionCancelConfirm", { count: 0 }));
    ui.unmount();

    const running = fakeTask(mission.id, "running", { required: false });
    const run = fakeRun(mission.id, running, "running");
    const dependent = fakeTask(mission.id, "planned", { depends_on: [running.id] });
    seedStore({ tasks: [running, dependent], runs: [run] });
    ui = renderUi(<RunDetail mission={mission} task={running} detailTab="changes" onDetailTabChange={noop} />);
    click(ui.container.querySelector("[data-testid=task-cancel]")!);
    expect(ui.container.querySelector("[data-testid=task-cancel]")?.textContent).toBe(t("missions.detail.actionCancel"));
    expect(ui.container.querySelector("[data-testid=task-cancel-confirm]")?.textContent).toContain(t("missions.detail.actionCancelConfirm", { count: 1 }));
    ui.unmount();
  });

  it("다시 시도·모델 변경이 같은 이유로 막히면 이유를 한 줄로 한 번만 보여 준다", () => {
    const mission = fakeMission();
    const task = fakeTask(mission.id, "failed");
    const run = fakeRun(mission.id, task, "running");
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    let ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector<HTMLButtonElement>("[data-testid=task-retry]")?.disabled).toBe(true);
    expect(ui.container.querySelector<HTMLButtonElement>("[data-testid=task-reassign]")?.disabled).toBe(true);
    let reasons = [...ui.container.querySelectorAll("[data-testid=task-disabled-reason]")].map((reason) => reason.textContent);
    expect(reasons).toEqual([t("missions.detail.disabled.running")]);
    ui.unmount();

    seedStore({ runs: [{ ...run, state: "stopping" }] });
    ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    reasons = [...ui.container.querySelectorAll("[data-testid=task-disabled-reason]")].map((reason) => reason.textContent);
    expect(reasons).toEqual([t("missions.detail.disabled.stopping")]);
    ui.unmount();
  });

  it("이미 다음 단계로 넘어간 취소 작업은 Lead 대화에서 대체 계획을 요청하도록 안내한다", () => {
    const mission = fakeMission({ phase: "reviewing" });
    const task = fakeTask(mission.id, "cancelled", { active_run_id: null });
    seedStore({ missions: [mission], tasks: [task] });
    const onOpenLead = vi.fn();
    let ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} onOpenLead={onOpenLead} />);
    expect(ui.container.querySelector<HTMLButtonElement>("[data-testid=task-retry]")?.disabled).toBe(true);
    const reasons = ui.container.querySelectorAll("[data-testid=task-disabled-reason]");
    expect(reasons).toHaveLength(1);
    expect(reasons[0].textContent).toContain(t("missions.detail.disabled.laterPhase"));
    const link = ui.container.querySelector("[data-testid=task-ask-lead]")!;
    expect(link.textContent).toBe(t("missions.detail.askLeadReplan"));
    click(link);
    expect(onOpenLead).toHaveBeenCalledTimes(1);
    ui.unmount();
    ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=task-disabled-reason]")?.textContent).toBe(t("missions.detail.disabled.laterPhase"));
    expect(ui.container.querySelector("[data-testid=task-ask-lead]")).toBeNull();
    ui.unmount();
  });

  it("모델 선택 설명은 실제 동작(실패 작업은 바로 재시도)과 같다", async () => {
    const client = installMockClient();
    const binding = { ...compatibleBinding(), label: "Alt", model_id: "alt-model" };
    const mission = fakeMission();
    mission.policy.allowed_binding_ids = [binding.id];
    client.bindingList = vi.fn(async () => ({ bindings: [binding] }));
    const task = fakeTask(mission.id, "failed");
    const run = fakeRun(mission.id, task, "failed");
    task.active_run_id = null;
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    click(ui.container.querySelector("[data-testid=task-reassign]")!);
    await flushAsync();
    expect(ui.container.querySelector<HTMLButtonElement>("[data-testid=reassign-picker] button")?.title)
      .toBe(t("missions.detail.reassignRetryTitle", { model: "alt-model" }));
    ui.unmount();
  });

  it.each(["verified", "failed"] as const)("미전송 호환성 실패는 설치 확인(%s)을 먼저 하고 확인 성공 시에만 재시도한다", async (installation) => {
    const client = installMockClient();
    const mission = fakeMission();
    const task = fakeTask(mission.id, "failed", { active_run_id: null });
    const binding = compatibleBinding();
    const run = fakeRun(mission.id, task, "failed", {
      binding_snapshot: binding,
      failure_code: "CAPABILITY_UNSUPPORTED",
      retry_evidence: { basis: "request_not_submitted", observed_at_unix_ms: "1", retry_after_unix_ms: null },
    });
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    const originalSync = useMissionStore.getState().syncMission;
    useMissionStore.setState({ syncMission: async () => undefined });
    const probe = vi.spyOn(client, "bindingProbe").mockResolvedValue({ binding, models: [], installation });
    const control = vi.spyOn(client, "missionTaskControl").mockImplementation(async () => {
      expect(probe).toHaveBeenCalledTimes(1);
      return result(mission);
    });
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    try {
      const retry = ui.container.querySelector<HTMLButtonElement>("[data-testid=task-retry]")!;
      expect(retry.textContent).toBe(t("missions.compatibility.recheckRetry"));
      click(retry);
      await flushAsync();
      expect(probe).toHaveBeenCalledWith({ binding_id: binding.id });
      expect(control).toHaveBeenCalledTimes(installation === "verified" ? 1 : 0);
    } finally {
      ui.unmount();
      useMissionStore.setState({ syncMission: originalSync });
    }
  });

  it("작업 제어 충돌은 재동기화 후 한 번 더 보내고, 다른 오류는 원인 문장과 자세히로 알린다", async () => {
    const client = installMockClient();
    const mission = fakeMission({ revision: "5" });
    const task = fakeTask(mission.id, "failed");
    const run = fakeRun(mission.id, task, "failed");
    task.active_run_id = null;
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    const original = useMissionStore.getState().syncMission;
    const sync = vi.fn(async () => {
      useMissionStore.setState((s) => ({ missions: { ...s.missions, [mission.id]: { ...mission, revision: "9" } } }));
    });
    useMissionStore.setState({ syncMission: sync });
    const control = vi.spyOn(client, "missionTaskControl")
      .mockRejectedValueOnce(new RpcClientError("REVISION_CONFLICT", "moved"))
      .mockResolvedValueOnce(result(mission))
      .mockRejectedValueOnce(new RpcClientError("POLICY_DENIED", "attempt budget exhausted"));
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    try {
      click(ui.container.querySelector("[data-testid=task-retry]")!);
      await flushAsync();
      expect(sync).toHaveBeenCalledTimes(1);
      expect(control.mock.calls.map((call) => call[0].expected_revision)).toEqual(["5", "9"]);
      expect(ui.container.querySelector("[role=alert]")).toBeNull();
      click(ui.container.querySelector("[data-testid=task-retry]")!);
      await flushAsync();
      const alerts = ui.container.querySelectorAll("[role=alert]");
      expect(alerts).toHaveLength(1);
      const message = alerts[0].querySelector("[data-testid=mission-error-message]")?.textContent ?? "";
      expect(message).toBe(t("missions.error.code.policyDenied"));
      expect(message).not.toContain("POLICY_DENIED");
      expect(alerts[0].querySelector("details")?.textContent).toContain("POLICY_DENIED: attempt budget exhausted");
    } finally {
      ui.unmount();
      useMissionStore.setState({ syncMission: original });
    }
  });
});

describe("RunDetail 제어 전제(자동 재전송)", () => {
  it("충돌 뒤 재동기화에서 할 일 상태·시도 수가 바뀌었으면 다시 시도를 재전송하지 않고 상태가 바뀌었다고 알린다", async () => {
    const client = installMockClient();
    const mission = fakeMission({ revision: "5" });
    const task = fakeTask(mission.id, "failed");
    const run = fakeRun(mission.id, task, "failed");
    task.active_run_id = null;
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    const original = useMissionStore.getState().syncMission;
    // 재동기화 사이에 데몬이 자동으로 새 시도를 시작했다.
    const sync = vi.fn(async () => {
      const next = fakeRun(mission.id, { ...task }, "running", { attempt: 2 });
      seedStore({
        missions: [{ ...mission, revision: "9" }],
        tasks: [{ ...task, state: "running", attempt_count: 2, active_run_id: next.id }],
        runs: [next],
      });
    });
    useMissionStore.setState({ syncMission: sync });
    const control = vi.spyOn(client, "missionTaskControl").mockRejectedValueOnce(new RpcClientError("REVISION_CONFLICT", "moved"));
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    try {
      click(ui.container.querySelector("[data-testid=task-retry]")!);
      await flushAsync();
      expect(sync).toHaveBeenCalledTimes(1);
      expect(control).toHaveBeenCalledTimes(1);
      expect(ui.container.querySelector("[data-testid=mission-error-message]")?.textContent).toBe(t("missions.error.stale"));
    } finally {
      ui.unmount();
      useMissionStore.setState({ syncMission: original });
    }
  });

  it("취소 확인창을 연 뒤 실행이 끝나 의미가 바뀌면 확인을 눌러도 보내지 않는다", async () => {
    const client = installMockClient();
    const mission = fakeMission();
    const task = fakeTask(mission.id, "running", { required: false });
    const run = fakeRun(mission.id, task, "running");
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    const control = vi.spyOn(client, "missionTaskControl").mockResolvedValue(result(mission));
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    click(ui.container.querySelector("[data-testid=task-cancel]")!);
    // 확인창이 떠 있는 동안 실행이 끝나 할 일이 실패로 바뀌었다.
    act(() => seedStore({ tasks: [{ ...task, state: "failed", active_run_id: null }], runs: [{ ...run, state: "failed" }] }));
    const ok = ui.container.querySelector("[data-testid=task-cancel-confirm] button.danger")!;
    click(ok);
    await flushAsync();
    expect(control).not.toHaveBeenCalled();
    expect(ui.container.querySelector("[data-testid=mission-error-message]")?.textContent).toBe(t("missions.error.stale"));
    ui.unmount();
  });

  it("모델 선택기를 연 뒤 할 일 배정이 바뀌면 고른 모델로 보내지 않는다", async () => {
    const client = installMockClient();
    const binding = { ...compatibleBinding(), label: "Alt", model_id: "alt-model" };
    const mission = fakeMission();
    mission.policy.allowed_binding_ids = [binding.id];
    client.bindingList = vi.fn(async () => ({ bindings: [binding] }));
    const task = fakeTask(mission.id, "failed");
    const run = fakeRun(mission.id, task, "failed");
    task.active_run_id = null;
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    const control = vi.spyOn(client, "missionTaskControl").mockResolvedValue(result(mission));
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    click(ui.container.querySelector("[data-testid=task-reassign]")!);
    await flushAsync();
    // 다른 창에서 같은 할 일의 모델을 먼저 바꿨다.
    act(() => seedStore({ tasks: [{ ...task, binding_id: "binding-other" }] }));
    click(ui.container.querySelector("[data-testid=reassign-picker] button")!);
    await flushAsync();
    expect(control).not.toHaveBeenCalled();
    expect(ui.container.querySelector("[data-testid=reassign-picker] [data-testid=mission-error-message]")?.textContent)
      .toBe(t("missions.error.stale"));
    ui.unmount();
  });

  it("동의한 버전과 관측 버전이 달라도 연결을 고를 수 있다(동의는 연결당 한 번)", async () => {
    const client = installMockClient();
    // 11 §3.4: 동의에 붙은 버전은 표시용이다 — 버전이 바뀌면 설정에서 `지금 확인`을 다시 누른다.
    const updated = { ...compatibleBinding(), label: "Updated", model_id: "updated-model", experimental_version: "2.1.0", runtime_version: "2.2.0" };
    const mission = fakeMission();
    mission.policy.allowed_binding_ids = [updated.id];
    client.bindingList = vi.fn(async () => ({ bindings: [updated] }));
    const task = fakeTask(mission.id, "failed");
    const run = fakeRun(mission.id, task, "failed");
    task.active_run_id = null;
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    const control = vi.spyOn(client, "missionTaskControl").mockResolvedValue(result(mission));
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    click(ui.container.querySelector("[data-testid=task-reassign]")!);
    await flushAsync();
    const button = [...ui.container.querySelectorAll<HTMLButtonElement>("[data-testid=reassign-picker] button")]
      .find((candidate) => candidate.textContent?.startsWith("Updated"))!;
    expect(button.disabled).toBe(false);
    expect(ui.container.querySelector("[data-testid=reassign-consent-stale]")).toBeNull();
    click(button);
    await flushAsync();
    expect(control).toHaveBeenCalledTimes(1);
    ui.unmount();
  });
});

describe("RunDetail 멈춘 실행 사용자 확인 정리(계약 C)", () => {
  function execFor(run: Run, overrides: Partial<ExecRecord> = {}): ExecRecord {
    return {
      id: run.exec_id!, mission_id: run.mission_id, run_id: run.id, state: "unknown",
      identity: { pid: 4242, start_token: "1", boot_id: "boot" }, group_kind: "observed_tree", group_reference: "pgid 4242",
      group_identity: null, resource_policy: newBinding().resource_policy, launch_manifest_ref: run.context_ref,
      owner_daemon_id: "previous-owner", started_at: run.started_at, ended_at: null, exit_code: null,
      ...overrides,
    };
  }

  function stuckRun() {
    const mission = fakeMission({ revision: "5" });
    const task = fakeTask(mission.id, "blocked", { blocked_code: "outcome_unknown" });
    const run = fakeRun(mission.id, task, "unknown", { exec_id: "exec-stuck" });
    const exec = execFor(run);
    seedStore({ missions: [mission], tasks: [task], runs: [run], execs: [exec] });
    return { mission, task, run };
  }

  it("데몬이 되찾아 감시할 수 있는 프로세스 그룹이면 직접 확인 버튼 대신 대기 안내만 보인다", () => {
    installMockClient();
    const mission = fakeMission({ revision: "5" });
    const task = fakeTask(mission.id, "blocked", { blocked_code: "outcome_unknown" });
    const run = fakeRun(mission.id, task, "unknown", { exec_id: "exec-guarded" });
    const exec = execFor(run, {
      group_identity: {
        kind: "macos_guardian",
        guardian: { pid: 99, start_token: "7", boot_id: "boot" },
        endpoint: "guardian.sock",
      },
    });
    seedStore({ missions: [mission], tasks: [task], runs: [run], execs: [exec] });
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=run-attest-supervised-line]")?.textContent).toBe(t("missions.attest.supervisedLine"));
    expect(ui.container.querySelector("[data-testid=run-attest-open]")).toBeNull();
    ui.unmount();
  });

  it("종료 증거 없는 실행은 확인 방법과 고지를 펼치고, 체크 전에는 보낼 수 없으며 계약 params로 보낸다", async () => {
    const client = installMockClient();
    const { mission, task, run } = stuckRun();
    const attest = vi.fn(async (_params: MissionRunAttestExitedParams): Promise<MutationResult> => result(mission));
    Object.assign(client, { missionRunAttestExited: attest });
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=run-attest-notice-line]")?.textContent).toBe(t("missions.attest.line"));
    expect(ui.container.querySelector("[data-testid=run-attest-panel]")).toBeNull();

    click(ui.container.querySelector("[data-testid=run-attest-open]")!);
    const panel = ui.container.querySelector("[data-testid=run-attest-panel]")!;
    expect(panel.querySelector("[data-testid=run-attest-pid]")?.textContent).toBe(t("missions.attest.pid", { pid: 4242 }));
    expect(panel.querySelector("[data-testid=run-attest-group]")?.textContent).toBe(t("missions.attest.group", { group: "pgid 4242" }));
    expect(panel.querySelector("[data-testid=run-attest-command]")?.textContent).toBe("ps -p 4242 -o pid,lstart,command");
    expect(panel.querySelector("[data-testid=run-attest-windows]")?.textContent).toBe(t("missions.attest.windowsHint"));
    expect(panel.textContent).toContain(t("missions.attest.check"));
    expect(panel.querySelector("[data-testid=run-attest-unverified]")?.textContent).toBe(t("missions.attest.unverified"));

    const submit = panel.querySelector<HTMLButtonElement>("[data-testid=run-attest-submit]")!;
    expect(submit.disabled).toBe(true);
    click(submit);
    await flushAsync();
    expect(attest).not.toHaveBeenCalled();

    click(panel.querySelector("[data-testid=run-attest-check]")!);
    expect(submit.disabled).toBe(false);
    click(submit);
    await flushAsync();
    expect(attest).toHaveBeenCalledTimes(1);
    expect(attest.mock.calls[0][0]).toEqual({
      request_id: expect.any(String),
      mission_id: mission.id,
      expected_revision: "5",
      run_id: run.id,
      attestation: "process_absent_confirmed",
    });
    expect(ui.container.querySelector("[data-testid=run-attest-panel]")).toBeNull();
    expect(ui.container.querySelector("[role=alert]")).toBeNull();
    expect(ui.container.querySelector("[role=status], [aria-live]")).toBeNull();
    ui.unmount();
  });

  it("PID 기록이 없으면 PID와 명령 대신 찾아보는 방법을 안내한다", () => {
    installMockClient();
    const mission = fakeMission();
    const task = fakeTask(mission.id, "blocked", { blocked_code: "outcome_unknown" });
    const run = fakeRun(mission.id, task, "interrupted");
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    click(ui.container.querySelector("[data-testid=run-attest-open]")!);
    expect(ui.container.querySelector("[data-testid=run-attest-pid]")).toBeNull();
    expect(ui.container.querySelector("[data-testid=run-attest-command]")).toBeNull();
    expect(ui.container.querySelector("[data-testid=run-attest-no-pid]")?.textContent).toBe(t("missions.attest.noPid"));
    expect(ui.container.querySelector("[data-testid=run-attest-unix]")?.textContent).toBe(t("missions.attest.unixHintNoPid"));
    ui.unmount();
  });

  it("데몬 거절(attestation_not_applicable)은 원인 문장과 다시 동기화 행동으로 알린다", async () => {
    const client = installMockClient();
    const { mission, task } = stuckRun();
    const attest = vi.fn(async (_params: MissionRunAttestExitedParams): Promise<MutationResult> => {
      throw new RpcClientError("INVALID_STATE", "run already reconciled", false, { reason_code: "attestation_not_applicable" });
    });
    Object.assign(client, { missionRunAttestExited: attest });
    const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    click(ui.container.querySelector("[data-testid=run-attest-open]")!);
    click(ui.container.querySelector("[data-testid=run-attest-check]")!);
    click(ui.container.querySelector("[data-testid=run-attest-submit]")!);
    await flushAsync();
    const alert = ui.container.querySelector("[data-testid=run-attest] [role=alert]")!;
    expect(alert.querySelector("[data-testid=mission-error-message]")?.textContent).toBe(t("missions.error.reason.attestationNotApplicable"));
    expect(alert.querySelector("[data-testid=mission-error-action]")?.getAttribute("data-action")).toBe("resync");
    expect(alert.textContent).not.toContain("attestation_not_applicable:");
    ui.unmount();
  });

  it("종료 증거가 있거나 live(멈추는 중 포함)면 동선이 없다 — 데몬 대상은 증거 없는 unknown/interrupted뿐", () => {
    installMockClient();
    const mission = fakeMission();
    const ended = fakeTask(mission.id, "blocked", { blocked_code: "outcome_unknown_ended" });
    const endedRun = fakeRun(mission.id, ended, "unknown");
    endedRun.reconciliation_ref = endedRun.context_ref;
    const running = fakeTask(mission.id, "running");
    const runningRun = fakeRun(mission.id, running, "running", { last_activity_at: "2026-09-13T01:00:00.000Z" });
    const stopping = fakeTask(mission.id, "running");
    const stoppingRun = fakeRun(mission.id, stopping, "stopping", { last_activity_at: "2026-09-13T01:00:00.000Z" });
    const interrupted = fakeTask(mission.id, "blocked", { blocked_code: "outcome_unknown" });
    const interruptedRun = fakeRun(mission.id, interrupted, "interrupted");
    seedStore({
      missions: [mission],
      tasks: [ended, running, stopping, interrupted],
      runs: [endedRun, runningRun, stoppingRun, interruptedRun],
    });
    for (const task of [ended, running, stopping]) {
      const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
      expect(ui.container.querySelector("[data-testid=run-attest]")).toBeNull();
      ui.unmount();
    }
    const ui = renderUi(<RunDetail mission={mission} task={interrupted} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=run-attest]")).not.toBeNull();
    ui.unmount();
  });

  it("사용자 확인으로 정리된 실행은 종료 관측 안내 대신 '직접 확인으로 정리됨 · 외부 영향 미확인'을 보인다", () => {
    installMockClient();
    const mission = fakeMission();
    const task = fakeTask(mission.id, "blocked", { blocked_code: "outcome_unknown_ended" });
    const run = fakeRun(mission.id, task, "interrupted", { reconciliation_kind: "user_attested" });
    run.reconciliation_ref = run.context_ref;
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    let ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=run-attested-notice-line]")?.textContent).toBe(t("missions.attest.label"));
    expect(ui.container.querySelector("[data-testid=recovery-notice]")).toBeNull();
    expect(ui.container.querySelector("[data-testid=run-attest]")).toBeNull();
    ui.unmount();

    // 관측한 종료(exec_exited)는 기존 종료 확인 안내 그대로.
    seedStore({ runs: [{ ...run, reconciliation_kind: "exec_exited" }] });
    ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=run-attested-notice]")).toBeNull();
    expect(ui.container.querySelector("[data-testid=recovery-notice]")).not.toBeNull();
    ui.unmount();
  });
});

describe("RunDetail 실험적 연결(계약 A)", () => {
  it("실험적 연결로 실행한 할 일은 헤더에 배지와 툴팁을 보이고, 일반 연결은 보이지 않는다", () => {
    const mission = fakeMission();
    const task = fakeTask(mission.id, "running");
    const experimental = compatibleBinding();
    experimental.capabilities.events = { supported: true, reason_code: "experimental_opt_in" };
    const run = fakeRun(mission.id, task, "running", { binding_snapshot: experimental });
    seedStore({ missions: [mission], tasks: [task], runs: [run] });
    let ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    const badge = ui.container.querySelector("[data-testid=detail-experimental-badge]")!;
    expect(badge.textContent).toContain(t("missions.experimental.badge"));
    expect(badge.getAttribute("title")).toBe(t("missions.experimentalRun.tooltip"));
    ui.unmount();

    seedStore({ runs: [{ ...run, binding_snapshot: compatibleBinding() }] });
    ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=detail-experimental-badge]")).toBeNull();
    ui.unmount();

    // 11 §8: 배지는 이 할 일에 필요한 기능만 본다 — 선택 기능의 동의로는 붙지 않는다.
    const optional = compatibleBinding();
    optional.capabilities.steer = { supported: true, reason_code: "experimental_opt_in" };
    optional.capabilities.resume = { supported: true, reason_code: "experimental_opt_in" };
    seedStore({ runs: [{ ...run, binding_snapshot: optional }] });
    ui = renderUi(<RunDetail mission={mission} task={task} detailTab="changes" onDetailTabChange={noop} />);
    expect(ui.container.querySelector("[data-testid=detail-experimental-badge]")).toBeNull();
    ui.unmount();
  });
});
