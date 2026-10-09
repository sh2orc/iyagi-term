import { beforeEach, expect, it } from "vitest";
import { PlanRepairNotice } from "./PlanRepairNotice";
import { fakeMission, fakeTask, fakeRun, renderUi, resetAllMissionState } from "./testSupport";
import { t, useI18nStore } from "../../i18n";

beforeEach(resetAllMissionState);

it.each(["ko", "en"] as const)("%s 계획 수리 대기와 자동 중단을 구분한다", language => {
  useI18nStore.setState({ language });
  const task = fakeTask(fakeMission().id, "blocked", { kind: "plan", role: "lead", blocked_code: "plan_format_repair", active_run_id: null });
  const run = fakeRun(task.mission_id, task, "failed", { retry_evidence: { basis: "plan_format_rejected", plan_revision: 0, rejected_result_ref: null } });
  const ui = renderUi(<PlanRepairNotice task={task} run={run} />);
  expect(ui.container.querySelector("[data-testid=plan-repair-notice-line]")?.textContent).toBe(t("missions.planRepair.short"));
  expect(ui.container.querySelector("details")?.textContent).toContain(t("missions.planRepair.waiting"));
  expect(ui.container.querySelector("button")).toBeNull();
  expect(ui.container.querySelector("[role=status], [aria-live]")).toBeNull();
  ui.unmount();
  const stopped = renderUi(<PlanRepairNotice task={{ ...task, state: "failed" }} run={run} />);
  expect(stopped.container.querySelector("[data-testid=plan-repair-notice-line]")?.textContent).toBe(t("missions.notice.planRepairStopped"));
  expect(stopped.container.querySelector("details")?.textContent).toContain(t("missions.planRepair.stopped"));
  stopped.unmount();
});

it("형식 수리 증거 없는 실패나 다른 담당과 종료 작업에는 안내를 표시하지 않는다", () => {
  const task = fakeTask(fakeMission().id, "blocked", { kind: "plan", role: "lead", blocked_code: "plan_format_repair", active_run_id: null });
  const run = fakeRun(task.mission_id, task, "failed", { retry_evidence: { basis: "plan_format_rejected", plan_revision: 0, rejected_result_ref: null } });
  for (const changed of [
    { task, run: { ...run, retry_evidence: null } },
    { task, run: { ...run, task_id: "another-task" } },
    { task: { ...task, kind: "implement" as const, role: "builder" as const }, run },
    { task: { ...task, state: "cancelled" as const }, run },
    { task, run: { ...run, state: "unknown" as const } },
  ]) {
    const ui = renderUi(<PlanRepairNotice {...changed} />);
    expect(ui.container.textContent).toBe("");
    ui.unmount();
  }
});
