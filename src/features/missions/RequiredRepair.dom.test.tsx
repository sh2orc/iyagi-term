import { compatibleBinding } from "./testSupport";
import { beforeEach, expect, it, vi } from "vitest";
import { RequiredRepairNotice } from "./RequiredRepairNotice";
import { RunDetail } from "./RunDetail";
import { click, fakeDecision, fakeMission, fakeTask, fakeRun, flushAsync, installMockClient, renderUi, resetAllMissionState, seedStore } from "./testSupport";
import { t, useI18nStore } from "../../i18n";
import { DecisionPanel } from "./DecisionPanel";

beforeEach(resetAllMissionState);

function setup() {
  const mission = fakeMission();
  const task = fakeTask(mission.id, "failed");
  const run = fakeRun(mission.id, task, "failed");
  const repair = fakeTask(mission.id, "ready", { kind: "plan", role: "lead", repair_cycle: 1, failure_repair_run_ids: [run.id] });
  seedStore({ missions: [mission], tasks: [task, repair], runs: [run] });
  return { mission, task, run, repair };
}

it.each(["ko", "en"] as const)("%s 실패 작업과 담당 Lead에 수정 계획을 표시한다", language => {
  useI18nStore.setState({ language });
  const { mission, task, repair } = setup();
  for (const shown of [task, repair]) {
    const ui = renderUi(<RequiredRepairNotice mission={mission} task={shown} />);
    const params = { cycle: 1, limit: mission.policy.max_repair_cycles };
    expect(ui.container.querySelector('[data-testid="required-repair-notice-line"]')?.textContent).toBe(t("missions.notice.requiredRepairWorking", params));
    expect(ui.container.querySelector("details")?.textContent).toContain(t("missions.requiredRepair.working", params));
    expect(ui.container.querySelector("[role=status], [aria-live]")).toBeNull();
    ui.unmount();
  }
  const stopped = renderUi(<RequiredRepairNotice mission={mission} task={{ ...repair, state: "failed" }} />);
  const stoppedParams = { cycle: 1, limit: mission.policy.max_repair_cycles };
  expect(stopped.container.querySelector('[data-testid="required-repair-notice-line"]')?.textContent).toBe(t("missions.notice.requiredRepairStopped", stoppedParams));
  expect(stopped.container.querySelector("details")?.textContent).toContain(t("missions.requiredRepair.stopped", stoppedParams));
  stopped.unmount();
});

it("Lead가 복구를 맡은 원래 작업은 중복 재시도를 막고 새 계획은 모델을 바꿀 수 있다", () => {
  const { mission, task, repair } = setup();
  const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="activity" onDetailTabChange={() => {}} />);
  expect(ui.container.querySelector<HTMLButtonElement>('[data-testid="task-retry"]')?.disabled).toBe(true);
  expect(ui.container.querySelector<HTMLButtonElement>('[data-testid="task-reassign"]')?.disabled).toBe(true);
  ui.unmount();
  const plan = renderUi(<RunDetail mission={mission} task={repair} detailTab="activity" onDetailTabChange={() => {}} />);
  expect(plan.container.querySelector<HTMLButtonElement>('[data-testid="task-reassign"]')?.disabled).toBe(false);
  expect(plan.container.querySelector('[data-testid="task-retry"]')).toBeNull();
  plan.unmount();
});

it("완료 미션·취소된 계획·다른 미션에 복구 진행을 표시하지 않는다", () => {
  const { mission, task, repair } = setup();
  for (const props of [
    { mission: { ...mission, state: "failed" as const }, task },
    { mission, task: { ...repair, state: "cancelled" as const } },
    { mission, task: { ...repair, mission_id: "different-mission" } },
  ]) {
    const ui = renderUi(<RequiredRepairNotice {...props} />);
    expect(ui.container.textContent).toBe("");
    ui.unmount();
  }
});

it("대기 중인 Lead의 모델 변경은 새 시도를 요청하지 않고 배정을 저장한다", async () => {
  const client = installMockClient();
  const { mission, repair } = setup();
  const binding = { ...compatibleBinding(), label: "Repair Lead model" };
  mission.policy.allowed_binding_ids = [binding.id];
  client.bindingList = vi.fn(async () => ({ bindings: [binding] }));
  const control = vi.spyOn(client, "missionTaskControl").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [repair.id] });
  const ui = renderUi(<RunDetail mission={mission} task={repair} detailTab="activity" onDetailTabChange={() => {}} />);
  click(ui.container.querySelector('[data-testid="task-reassign"]')!);
  await flushAsync();
  click([...ui.container.querySelectorAll('[data-testid="reassign-picker"] button')].find(b => b.textContent?.includes(binding.label))!);
  await flushAsync();
  expect(control).toHaveBeenCalledTimes(1);
  expect(control.mock.calls[0][0]).toMatchObject({ task_id: repair.id, action: "reassign", binding_id: binding.id, expected_revision: mission.revision });
  ui.unmount();
});

it.each(["ko", "en"] as const)("%s 리뷰 수정 한도에서는 근거 해제·설정 변경·중단 동선을 안내한다", async language => {
  useI18nStore.setState({ language });
  const client = installMockClient();
  const mission = fakeMission();
  const decision = fakeDecision(mission.id, { kind: "budget", options: [{ id: "stop_review_repair", label: "Host stop" }] });
  const answer = vi.spyOn(client, "missionDecisionAnswer").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [decision.id] });
  seedStore({ missions: [mission], decisions: [decision] });
  const ui = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => {}} />);
  expect(ui.container.querySelector('[data-testid="decision-question"]')?.textContent).toBe(t("missions.decision.situation.reviewRepair"));
  expect(ui.container.querySelector('[data-testid="decision-details"]')?.textContent).toContain(t("missions.requiredRepair.reviewExhausted"));
  // 리뷰 수정 한도도 한도 결정이다 — 한도 편집기를 열 수 있다.
  expect(ui.container.querySelector('[data-testid="decision-adjust-limits"]')).not.toBeNull();
  const stop = ui.container.querySelector('[data-testid="decision-option"]')!;
  expect(stop.textContent).toBe(t("missions.requiredRepair.stopReview"));
  click(stop);
  await flushAsync();
  expect(answer.mock.calls[0][0]).toMatchObject({ decision_id: decision.id, option_id: "stop_review_repair", expected_revision: mission.revision });
  ui.unmount();
});
