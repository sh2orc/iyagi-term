import { compatibleBinding } from "./testSupport";
import { beforeEach, expect, it, vi } from "vitest";
import { RunDetail } from "./RunDetail";
import { DecisionPanel } from "./DecisionPanel";
import { RecoveryNotice } from "./RecoveryNotice";
import { newBinding } from "./configuration";
import { useI18nStore, t } from "../../i18n";
import { click, fakeDecision, fakeMission, fakeRun, fakeTask, flushAsync, installMockClient, renderUi, resetAllMissionState, seedStore } from "./testSupport";

beforeEach(resetAllMissionState);

it.each(["ko", "en"] as const)("%s 충돌 후보 제외는 기록 보존과 필수 대체 계획을 안내한다", async language => {
  useI18nStore.setState({ language });
  const client = installMockClient();
  const mission = fakeMission({ phase: "integrating" });
  const task = fakeTask(mission.id, "failed", { kind: "integrate", role: "integrator", active_run_id: null });
  const run = fakeRun(mission.id, task, "failed");
  const decision = fakeDecision(mission.id, { kind: "conflict", requesting_run_id: run.id, affected_task_ids: [task.id],
    options: [{ id: "exclude_candidate", label: "Host exclude" }] });
  seedStore({ missions: [mission], tasks: [task], runs: [run], decisions: [decision] });
  const answer = vi.spyOn(client, "missionDecisionAnswer").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [decision.id] });
  const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
  expect(handle.container.querySelector("[data-testid=integration-exclusion-note]")?.textContent).toBe(t("missions.integration.excludeNote"));
  const button = handle.container.querySelector("[data-testid=decision-option]")!;
  expect(button.textContent).toBe(t("missions.integration.exclude"));
  click(button); await flushAsync();
  expect(answer.mock.calls[0][0]).toMatchObject({ decision_id: decision.id, option_id: "exclude_candidate", expected_revision: mission.revision });
  handle.unmount();
});

it.each(["ko", "en"] as const)("%s 취소한 필수 작업은 조건 보존 안내와 명시적 재시도를 제공한다", async language => {
  useI18nStore.setState({ language });
  const client = installMockClient();
  const mission = fakeMission({ phase: "implementing" });
  const task = fakeTask(mission.id, "cancelled", { required: true, active_run_id: null });
  seedStore({ missions: [mission], tasks: [task] });
  const retry = vi.spyOn(client, "missionTaskControl").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [task.id] });
  const handle = renderUi(<RunDetail mission={mission} task={task} detailTab="activity" onDetailTabChange={() => undefined} />);
  expect(handle.container.querySelector("[data-testid=required-cancelled-notice]")?.textContent).toBe(t("missions.detail.requiredCancelled"));
  const button = handle.container.querySelector<HTMLButtonElement>("[data-testid=task-retry]")!;
  expect(button.disabled).toBe(false);
  click(button); await flushAsync();
  expect(retry.mock.calls[0][0]).toMatchObject({ action: "retry", task_id: task.id, binding_id: null });
  handle.unmount();
});

it.each(["stopping", "pending-cleanup", "later-phase"])("취소 작업의 %s 상태에서는 재시도와 모델 변경을 막는다", mode => {
  installMockClient();
  const mission = fakeMission({ phase: mode === "later-phase" ? "reviewing" : "implementing" });
  const task = fakeTask(mission.id, "cancelled", { active_run_id: mode === "stopping" ? "owned-run" : null });
  const run = fakeRun(mission.id, task, mode === "pending-cleanup" ? "cancelled" : "stopping");
  if (mode === "later-phase") task.active_run_id = null;
  seedStore({ missions: [mission], tasks: [task], runs: mode !== "later-phase" ? [run] : [] });
  const handle = renderUi(<RunDetail mission={mission} task={task} detailTab="activity" onDetailTabChange={() => undefined} />);
  expect(handle.container.querySelector<HTMLButtonElement>("[data-testid=task-retry]")?.disabled).toBe(true);
  expect(handle.container.querySelector<HTMLButtonElement>("[data-testid=task-reassign]")?.disabled).toBe(true);
  handle.unmount();
});

it.each(["ko", "en"] as const)("%s 불명 통합은 격리 결과를 보존하고 새 공간에서 재구성하도록 안내한다", async (language) => {
  useI18nStore.setState({ language });
  const client = installMockClient();
  const mission = fakeMission({ phase: "integrating" });
  const task = fakeTask(mission.id, "blocked", { kind: "integrate", role: "integrator", blocked_code: "outcome_unknown_ended", active_run_id: null });
  task.integration = { plan_ref: task.contract.objective_ref, step: { resolving: { conflict_run_id: "original-conflict" } } };
  const run = fakeRun(mission.id, task, "unknown");
  run.reconciliation_ref = run.context_ref;
  const decision = fakeDecision(mission.id, { kind: "recovery", requesting_run_id: run.id, affected_task_ids: [task.id],
    options: [{ id: "retry_reconciled_task", label: "Host retry" }, { id: "stop_reconciled_mission", label: "Host stop" }] });
  seedStore({ missions: [mission], tasks: [task], runs: [run], decisions: [decision] });
  const answer = vi.spyOn(client, "missionDecisionAnswer").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [decision.id] });
  const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
  const attempt = { task: task.title, attempt: task.attempt_count, limit: mission.policy.max_attempts_per_task };
  expect(handle.container.querySelector("[data-testid=decision-question]")?.textContent).toBe(t("missions.decision.situation.integrationRecovery", attempt));
  expect(handle.container.querySelector("[data-testid=decision-details]")?.textContent).toContain(t("missions.integration.recoveryQuestion", attempt));
  const button = handle.container.querySelector("[data-testid=decision-option]")!;
  expect(button.textContent).toBe(t("missions.integration.rebuild"));
  click(button); await flushAsync();
  expect(answer.mock.calls[0][0]).toMatchObject({ decision_id: decision.id, option_id: "retry_reconciled_task" });
  handle.unmount();
});

it.each(["ko", "en"] as const)("%s 통합 담당과 충돌 해결 요청을 표시하고 실행 없이 담당만 변경한다", async language => {
  useI18nStore.setState({ language });
  const client = installMockClient();
  const binding = { ...compatibleBinding(), label: "Assigned integrator", model_id: "integrator-model" };
  const replacement = { ...compatibleBinding(), label: "Replacement integrator", model_id: "replacement-model" };
  client.bindingList = vi.fn(async () => ({ bindings: [binding, replacement] }));
  const mission = fakeMission({ phase: "integrating" });
  mission.policy.allowed_binding_ids = [binding.id, replacement.id];
  const task = fakeTask(mission.id, "failed", { kind: "integrate", role: "integrator", binding_id: binding.id, active_run_id: null });
  task.integration = { plan_ref: task.contract.objective_ref, step: "automatic" };
  const run = fakeRun(mission.id, task, "failed"); run.binding_snapshot = null; run.requested_model = null;
  const decision = fakeDecision(mission.id, { kind: "conflict", requesting_run_id: run.id, affected_task_ids: [task.id],
    options: [{ id: "resolve_and_reintegrate", label: "Host resolution" }, { id: "stop_mission", label: "Host stop" }] });
  seedStore({ missions: [mission], tasks: [task], runs: [run], decisions: [decision] });
  const control = vi.spyOn(client, "missionTaskControl").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [task.id] });
  const answer = vi.spyOn(client, "missionDecisionAnswer").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [decision.id] });
  const handle = renderUi(<><RunDetail mission={mission} task={task} detailTab="activity" onDetailTabChange={() => undefined} />
    <DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} /></>);
  await flushAsync();
  expect(handle.container.querySelector("[data-testid=integration-assignment]")?.textContent).toContain("Assigned integrator · integrator-model");
  expect(handle.container.querySelector("[data-testid=detail-model]")?.textContent).toBe(t("missions.detail.noModel"));
  expect((handle.container.querySelector("[data-testid=task-retry]") as HTMLButtonElement).disabled).toBe(true);
  click(handle.container.querySelector("[data-testid=task-reassign]")!); await flushAsync();
  click([...handle.container.querySelectorAll("[data-testid=reassign-picker] button")].find(b => b.textContent?.includes(replacement.label))!);
  await flushAsync();
  expect(control.mock.calls[0][0]).toMatchObject({ action: "reassign", task_id: task.id, binding_id: replacement.id });
  expect(handle.container.querySelector("[data-testid=decision-question]")?.textContent).toBe(t("missions.decision.situation.conflict"));
  expect(handle.container.querySelector("[data-testid=decision-details]")?.textContent).toContain(t("missions.integration.question"));
  click([...handle.container.querySelectorAll("[data-testid=decision-option]")].find(b => b.textContent === t("missions.integration.resolve"))!);
  await flushAsync();
  expect(answer.mock.calls[0][0]).toMatchObject({ decision_id: decision.id, option_id: "resolve_and_reintegrate" });
  handle.unmount();
});

it("자동 통합 충돌의 결정을 기다리는 동안 같은 입력의 재시도를 막는다", () => {
  installMockClient();
  const mission = fakeMission({ phase: "integrating" });
  const task = fakeTask(mission.id, "failed", { kind: "integrate", role: null, binding_id: null, active_run_id: null });
  const run = fakeRun(mission.id, task, "failed");
  run.binding_snapshot = null;
  const decision = fakeDecision(mission.id, { kind: "conflict", requesting_run_id: run.id, affected_task_ids: [task.id] });
  seedStore({ missions: [mission], tasks: [task], runs: [run], decisions: [decision] });
  const handle = renderUi(<RunDetail mission={mission} task={task} detailTab="activity" onDetailTabChange={() => undefined} />);
  expect((handle.container.querySelector("[data-testid='task-retry']") as HTMLButtonElement).disabled).toBe(true);
  handle.unmount();
});

it.each(["ko", "en"] as const)("%s 취소한 자동 통합은 명시적으로 재시도하며 모델/지시 입력을 제공하지 않는다", async (language) => {
  useI18nStore.setState({ language });
  const client = installMockClient();
  const mission = fakeMission({ phase: "integrating" });
  const task = fakeTask(mission.id, "cancelled", { kind: "integrate", role: null, binding_id: null, active_run_id: null });
  const run = fakeRun(mission.id, task, "cancelled");
  task.active_run_id = null;
  run.binding_snapshot = null;
  seedStore({ missions: [mission], tasks: [task], runs: [run] });
  const retry = vi.spyOn(client, "missionTaskControl").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [task.id] });
  const handle = renderUi(<RunDetail mission={mission} task={task} detailTab="activity" onDetailTabChange={() => undefined} />);
  expect(handle.container.querySelector("[data-testid='integration-instructions']")?.textContent).toBe(t("missions.detail.integrationInstructions"));
  expect(handle.container.querySelector("[data-testid='task-composer-input']")).toBeNull();
  expect(handle.container.querySelector("[data-testid='task-reassign']")).toBeNull();
  click(handle.container.querySelector("[data-testid='task-retry']")!);
  await flushAsync();
  expect(retry.mock.calls[0][0]).toMatchObject({ task_id: task.id, action: "retry", binding_id: null });
  handle.unmount();
});

it.each(["ko", "en"] as const)("%s 종료 증거와 명시적 새 시도 선택을 표시한다", async (language) => {
  useI18nStore.setState({ language });
  const client = installMockClient();
  const mission = fakeMission();
  const task = fakeTask(mission.id, "blocked", { blocked_code: "outcome_unknown_ended", active_run_id: null });
  const run = fakeRun(mission.id, task, "unknown");
  run.reconciliation_ref = run.context_ref;
  const decision = fakeDecision(mission.id, { kind: "recovery", requesting_run_id: run.id, affected_task_ids: [task.id],
    options: [{ id: "retry_reconciled_task", label: "Host retry" }, { id: "stop_reconciled_mission", label: "Host stop" }] });
  seedStore({ missions: [mission], tasks: [task], runs: [run], decisions: [decision] });
  const answer = vi.spyOn(client, "missionDecisionAnswer").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [decision.id] });
  const handle = renderUi(<><RecoveryNotice run={run} /><DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} /></>);
  expect(handle.container.querySelector("[data-testid='recovery-notice-line']")?.textContent).toBe(t("missions.recovery.summary"));
  expect(handle.container.querySelector("[data-testid='recovery-notice']")?.textContent).toContain(t("missions.recovery.ended"));
  expect(handle.container.querySelector("[data-testid='decision-question']")?.textContent).toContain(`${task.attempt_count}/${mission.policy.max_attempts_per_task}`);
  const buttons = handle.container.querySelectorAll("[data-testid='decision-option']");
  expect(buttons[0].textContent).toBe(t("missions.recovery.retry"));
  expect(buttons[1].textContent).toBe(t("missions.recovery.stop"));
  click(buttons[0]);
  await flushAsync();
  expect(answer.mock.calls[0][0]).toMatchObject({ decision_id: decision.id, option_id: "retry_reconciled_task", expected_revision: mission.revision });
  handle.unmount();
});

it("종료 미확인 안내를 표시하고 확인된 실행에서만 모델을 변경한다", async () => {
  const client = installMockClient();
  const mission = fakeMission();
  const task = fakeTask(mission.id, "blocked", { blocked_code: "outcome_unknown_ended", active_run_id: null });
  const run = fakeRun(mission.id, task, "unknown");
  seedStore({ missions: [mission], tasks: [task], runs: [run] });
  let handle = renderUi(<RunDetail mission={mission} task={task} detailTab="exec" onDetailTabChange={() => undefined} />);
  expect(handle.container.querySelector("[data-testid='recovery-notice-line']")?.textContent).toBe(t("missions.notice.recoveryUnconfirmed"));
  expect(handle.container.querySelector("[data-testid='recovery-notice']")?.textContent).toContain(t("missions.recovery.unconfirmed"));
  expect((handle.container.querySelector("[data-testid='task-reassign']") as HTMLButtonElement).disabled).toBe(true);
  handle.unmount();
  run.reconciliation_ref = run.context_ref;
  const binding = { ...compatibleBinding(), label: "Recovered connection" };
  mission.policy.allowed_binding_ids = [binding.id];
  client.bindingList = vi.fn(async () => ({ bindings: [binding] }));
  const control = vi.spyOn(client, "missionTaskControl").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [task.id] });
  seedStore({ missions: [mission], tasks: [task], runs: [run] });
  handle = renderUi(<RunDetail mission={mission} task={task} detailTab="exec" onDetailTabChange={() => undefined} />);
  expect(handle.container.querySelector("[data-testid='task-retry']")).toBeNull();
  click(handle.container.querySelector("[data-testid='task-reassign']")!);
  await flushAsync();
  click([...handle.container.querySelectorAll("[data-testid='reassign-picker'] button")].find(b => b.textContent?.includes(binding.label))!);
  await flushAsync();
  expect(control.mock.calls[0][0]).toMatchObject({ action: "reassign", binding_id: binding.id });
  handle.unmount();
});

it("모델 변경 후 재시도는 허용된 모델만 표시하고 한 요청으로 저장한다", async () => {
  const client = installMockClient();
  const binding = { ...compatibleBinding(), label: "Recovery model", model_id: "replacement" };
  const outside = { ...compatibleBinding(), label: "Outside policy" };
  const disabled = { ...newBinding(), enabled: false, label: "Disabled model" };
  const mission = fakeMission();
  mission.policy.allowed_binding_ids = [binding.id, disabled.id];
  const task = fakeTask(mission.id, "failed");
  const run = fakeRun(mission.id, task, "failed", { failure_code: "AUTH_REQUIRED", ended_at: "2026-09-15T00:00:00.000Z" });
  task.active_run_id = null;
  seedStore({ missions: [mission], tasks: [task], runs: [run] });
  client.bindingList = vi.fn(async () => ({ bindings: [binding, outside, disabled] }));
  const control = vi.spyOn(client, "missionTaskControl").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [task.id] });
  const handle = renderUi(<RunDetail mission={mission} task={task} detailTab="exec" onDetailTabChange={() => undefined} />);
  click(handle.container.querySelector("[data-testid='task-reassign']")!);
  await flushAsync();
  const picker = handle.container.querySelector("[data-testid='reassign-picker']")!;
  expect(picker.textContent).toContain("Recovery model");
  expect(picker.textContent).not.toContain("Outside policy");
  expect(picker.textContent).not.toContain("Disabled model");
  click([...picker.querySelectorAll("button")].find((b) => b.textContent?.includes("Recovery model"))!);
  await flushAsync();
  expect(control).toHaveBeenCalledTimes(1);
  expect(control.mock.calls[0][0]).toMatchObject({ mission_id: mission.id, task_id: task.id,
    action: "retry", binding_id: binding.id, expected_revision: mission.revision });
  expect(handle.container.querySelector("[role='alert']")).toBeNull();
  handle.unmount();
});

it.each(["unknown", "interrupted", "running"] as const)("소유 실행이 %s이면 모델 변경과 재시도를 막는다", (state) => {
  const mission = fakeMission();
  const task = fakeTask(mission.id, "failed");
  const run = fakeRun(mission.id, task, state);
  seedStore({ missions: [mission], tasks: [task], runs: [run] });
  const handle = renderUi(<RunDetail mission={mission} task={task} detailTab="exec" onDetailTabChange={() => undefined} />);
  expect((handle.container.querySelector("[data-testid='task-reassign']") as HTMLButtonElement).disabled).toBe(true);
  expect((handle.container.querySelector("[data-testid='task-retry']") as HTMLButtonElement).disabled).toBe(true);
  expect(handle.container.querySelector("[data-testid='detail-elapsed']")?.textContent).toBe("0:01:00");
  // 저장 시각 설명은 활성 시간 옆 설명 아이콘(툴팁)으로만 둔다.
  expect(handle.container.querySelector("[data-testid='detail-saved-note']")?.getAttribute("title")).toBe(t("missions.time.savedNote"));
  expect(handle.container.textContent).not.toContain(t("missions.time.savedNote"));
  handle.unmount();
});

it.each(["succeeded", "superseded"] as const)("%s 작업을 재시도 가능한 실패로 표시하지 않는다", (state) => {
  const mission = fakeMission();
  const task = fakeTask(mission.id, state);
  seedStore({ missions: [mission], tasks: [task] });
  const handle = renderUi(<RunDetail mission={mission} task={task} detailTab="exec" onDetailTabChange={() => undefined} />);
  expect(handle.container.querySelector("[data-testid='task-retry']")).toBeNull();
  expect((handle.container.querySelector("[data-testid='task-reassign']") as HTMLButtonElement).disabled).toBe(true);
  handle.unmount();
});

it.each(["ko", "en"] as const)("%s 실패 결정에 원인·시도 한도·복구 선택지를 표시한다", async (language) => {
  useI18nStore.setState({ language });
  const client = installMockClient();
  const mission = fakeMission();
  const task = fakeTask(mission.id, "failed", { title: "Prepare deployment" });
  const run = fakeRun(mission.id, task, "failed", { failure_code: "AUTH_REQUIRED" });
  const decision = fakeDecision(mission.id, { kind: "recovery", affected_task_ids: [task.id], requesting_run_id: run.id,
    options: [{ id: "retry_failed_task", label: "Host retry" }, { id: "stop_failed_mission", label: "Host stop" }] });
  seedStore({ missions: [mission], tasks: [task], runs: [run], decisions: [decision] });
  const answer = vi.spyOn(client, "missionDecisionAnswer").mockResolvedValue({ mission_id: mission.id, revision: "6", event_seq: "6", entity_ids: [decision.id] });
  const handle = renderUi(<DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} />);
  const question = handle.container.querySelector("[data-testid='decision-question']")!.textContent;
  expect(question).toContain("Prepare deployment");
  expect(question).toContain(t("missions.blocked.authRequired"));
  expect(question).toContain(`${task.attempt_count}/${mission.policy.max_attempts_per_task}`);
  // 원문 code와 긴 복구 안내는 자세히 안에만 둔다.
  expect(question).not.toContain("AUTH_REQUIRED");
  expect(handle.container.querySelector("[data-testid='decision-details']")?.textContent).toContain("AUTH_REQUIRED");
  const buttons = handle.container.querySelectorAll("[data-testid='decision-option']");
  expect(buttons[0].textContent).toBe(t("missions.decision.retryFailedTask"));
  expect(buttons[1].textContent).toBe(t("missions.decision.stopFailedMission"));
  click(buttons[0]);
  await flushAsync();
  expect(answer.mock.calls[0][0]).toMatchObject({ decision_id: decision.id, option_id: "retry_failed_task" });
  handle.unmount();
});


it.each(["ko", "en"] as const)("%s 관찰 기반 종료 범위를 상세와 재시도 결정에 표시한다", (language) => {
  useI18nStore.setState({ language });
  installMockClient();
  const mission = fakeMission();
  const task = fakeTask(mission.id, "blocked", { blocked_code: "outcome_unknown_ended" });
  const run = fakeRun(mission.id, task, "unknown", { exec_id: "owned-exec" });
  run.reconciliation_ref = run.context_ref;
  const exec = { id: run.exec_id!, mission_id: mission.id, run_id: run.id, state: "exited" as const,
    identity: null, group_kind: "observed_tree" as const, group_reference: "private-observer", group_identity: null,
    resource_policy: newBinding().resource_policy, launch_manifest_ref: run.context_ref, owner_daemon_id: "previous-owner",
    started_at: run.started_at, ended_at: "2026-09-16T00:00:00Z", exit_code: null };
  const decision = fakeDecision(mission.id, { kind: "recovery", requesting_run_id: run.id, affected_task_ids: [task.id],
    options: [{ id: "retry_reconciled_task", label: "retry" }, { id: "stop_reconciled_mission", label: "stop" }] });
  seedStore({ missions: [mission], tasks: [task], runs: [run], execs: [exec], decisions: [decision] });
  let handle = renderUi(<><RecoveryNotice run={run} /><DecisionPanel mission={mission} decision={decision} currentDecisionId={null} onJumpToCurrent={() => undefined} /></>);
  const notices = handle.container.querySelectorAll("[data-testid='recovery-coverage-notice']");
  expect(notices).toHaveLength(2);
  for (const notice of notices) {
    expect(notice.querySelector("[data-testid='recovery-coverage-notice-line']")?.textContent).toBe(t("missions.notice.recoveryCoverage"));
    expect(notice.textContent).toContain(t("missions.recovery.observedCoverage"));
  }
  handle.unmount();
  seedStore({ execs: [{ ...exec, mission_id: "another-mission" }] });
  handle = renderUi(<RecoveryNotice run={run} />);
  expect(handle.container.querySelector("[data-testid='recovery-coverage-notice']")).toBeNull();
  handle.unmount();
  seedStore({ execs: [{ ...exec, group_kind: "cgroup" }] });
  handle = renderUi(<RecoveryNotice run={run} />);
  expect(handle.container.querySelector("[data-testid='recovery-coverage-notice']")).toBeNull();
  handle.unmount();
});
