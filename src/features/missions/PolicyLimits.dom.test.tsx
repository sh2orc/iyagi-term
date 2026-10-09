/**
 * PolicyLimitsEditor: 현재 사용량/한도 표시, policy_ceiling clamp, 분/시간 입력,
 * 건드린 필드만 최신 정책 위에 저장, REVISION_CONFLICT 재동기화.
 */

import { act } from "react";
import { beforeEach, expect, it, vi } from "vitest";
import { RpcClientError } from "../daemon/client";
import { t } from "../../i18n";
import { PolicyLimitsEditor } from "./CostPolicyEditor";
import { useMissionStore } from "./store";
import { click, fakeMission, fakeRun, fakeTask, flushAsync, installMockClient, renderUi, resetAllMissionState, seedStore, setValue } from "./testSupport";

beforeEach(resetAllMissionState);

const result = (missionId: string) => ({ mission_id: missionId, revision: "6", event_seq: "6", entity_ids: [] });

it("현재 사용량과 한도, 최대값을 보여 주고 실행 중/일시정지가 아니면 저장할 수 없다", () => {
  installMockClient();
  const mission = fakeMission({ active_time_ms: "5400000", automatic_start_count: 3 });
  mission.policy.active_time_limit_ms = "7200000";
  mission.policy.max_automatic_starts = 10;
  const task = fakeTask(mission.id, "running", { attempt_count: 2, repair_cycle: 1 });
  const run = fakeRun(mission.id, task, "running");
  seedStore({ missions: [mission], tasks: [task], runs: [run] });
  let ui = renderUi(<PolicyLimitsEditor mission={mission} />);
  const text = (field: string) => ui.container.querySelector(`[data-testid=limits-row-${field}]`)?.textContent ?? "";
  expect(text("active")).toContain(t("missions.limits.usage", { used: "1:30:00", limit: "2:00:00" }));
  expect(text("active")).toContain(t("missions.limits.max", { max: "24:00:00" }));
  expect(text("starts")).toContain(t("missions.limits.usage", { used: 3, limit: 10 }));
  expect(text("attempts")).toContain(t("missions.limits.usage", { used: 2, limit: mission.policy.max_attempts_per_task }));
  expect(text("repair")).toContain(t("missions.limits.usage", { used: 1, limit: mission.policy.max_repair_cycles }));
  expect(text("parallel")).toContain(t("missions.limits.usageParallel", { used: 1, limit: mission.policy.max_parallel_runs }));
  expect(text("parallel")).toContain(t("missions.limits.max", { max: 8 }));
  // 값을 바꾸기 전에는 저장 버튼이 비활성이다.
  expect(ui.container.querySelector<HTMLButtonElement>("[data-testid=limits-save]")!.disabled).toBe(true);
  ui.unmount();
  const done = { ...mission, state: "completed" as const };
  seedStore({ missions: [done] });
  ui = renderUi(<PolicyLimitsEditor mission={done} />);
  expect(ui.container.textContent).toContain(t("missions.limits.notEditable"));
  expect(ui.container.querySelector<HTMLInputElement>("[data-testid=limits-parallel]")!.disabled).toBe(true);
  ui.unmount();
});

it("입력을 policy_ceiling으로 맞추고 분 단위 시간을 ms로 바꿔 건드린 필드만 저장한다", async () => {
  const client = installMockClient();
  const mission = fakeMission();
  mission.policy.active_time_limit_ms = "5400000"; // 90분 → 분 단위로 시작
  seedStore({ missions: [mission] });
  const update = vi.spyOn(client, "missionPolicyUpdate").mockResolvedValue(result(mission.id));
  const ui = renderUi(<PolicyLimitsEditor mission={mission} onClose={() => undefined} />);
  const field = <T extends HTMLElement>(id: string) => ui.container.querySelector<T>(`[data-testid=limits-${id}]`)!;
  expect(field<HTMLSelectElement>("active-unit").value).toBe("minutes");
  expect(field<HTMLInputElement>("active").value).toBe("90");
  setValue(field<HTMLInputElement>("active"), "120");
  setValue(field<HTMLInputElement>("parallel"), "99");
  setValue(field<HTMLInputElement>("attempts"), "0");
  setValue(field<HTMLInputElement>("cost"), "2.5");
  // 저장 전 다른 곳에서 수정 사이클과 revision이 바뀌었다.
  act(() => useMissionStore.setState((s) => ({ missions: { ...s.missions,
    [mission.id]: { ...mission, revision: "9", policy: { ...mission.policy, max_repair_cycles: 5 } } } })));
  click(field("save"));
  await flushAsync();
  expect(update).toHaveBeenCalledTimes(1);
  const params = update.mock.calls[0][0];
  expect(params.expected_revision).toBe("9");
  expect(params.policy).toMatchObject({
    active_time_limit_ms: "7200000",
    max_parallel_runs: 8,
    max_attempts_per_task: 1,
    max_cost_usd_micros: "2500000",
    max_repair_cycles: 5,
    max_automatic_starts: mission.policy.max_automatic_starts,
    unknown_cost: mission.policy.unknown_cost,
  });
  expect(field<HTMLInputElement>("parallel").value).toBe("8");
  expect(field<HTMLInputElement>("attempts").value).toBe("1");
  expect(ui.container.querySelector("[data-testid=limits-clamped]")?.textContent).toBe(t("missions.limits.clamped"));
  expect(ui.container.querySelector("[data-testid=limits-saved]")?.textContent).toBe(t("missions.limits.saved"));
  ui.unmount();
});

it("시간 단위를 바꾸면 같은 시간을 유지하고 24시간 상한으로 맞춘다", async () => {
  const client = installMockClient();
  const mission = fakeMission();
  mission.policy.active_time_limit_ms = "7200000";
  seedStore({ missions: [mission] });
  const update = vi.spyOn(client, "missionPolicyUpdate").mockResolvedValue(result(mission.id));
  const ui = renderUi(<PolicyLimitsEditor mission={mission} />);
  const active = ui.container.querySelector<HTMLInputElement>("[data-testid=limits-active]")!;
  const unit = ui.container.querySelector<HTMLSelectElement>("[data-testid=limits-active-unit]")!;
  expect(unit.value).toBe("hours");
  expect(active.value).toBe("2");
  setValue(unit, "minutes");
  expect(active.value).toBe("120");
  setValue(unit, "hours");
  setValue(active, "30");
  click(ui.container.querySelector("[data-testid=limits-save]")!);
  await flushAsync();
  expect(update.mock.calls[0][0].policy.active_time_limit_ms).toBe("86400000");
  expect(active.value).toBe("24");
  ui.unmount();
});

it("정수가 아닌 한도와 잘못된 비용은 요청 없이 알린다", async () => {
  const client = installMockClient();
  const mission = fakeMission();
  seedStore({ missions: [mission] });
  const update = vi.spyOn(client, "missionPolicyUpdate").mockResolvedValue(result(mission.id));
  const ui = renderUi(<PolicyLimitsEditor mission={mission} />);
  setValue(ui.container.querySelector<HTMLInputElement>("[data-testid=limits-starts]")!, "");
  click(ui.container.querySelector("[data-testid=limits-save]")!);
  await flushAsync();
  expect(ui.container.querySelector("[role=alert]")?.textContent).toBe(t("missions.limits.invalidNumber"));
  setValue(ui.container.querySelector<HTMLInputElement>("[data-testid=limits-starts]")!, "3");
  setValue(ui.container.querySelector<HTMLInputElement>("[data-testid=limits-cost]")!, "-1");
  click(ui.container.querySelector("[data-testid=limits-save]")!);
  await flushAsync();
  expect(ui.container.querySelector("[role=alert]")?.textContent).toBe(t("missions.create.costInvalid"));
  expect(update).not.toHaveBeenCalled();
  ui.unmount();
});

it("REVISION_CONFLICT면 재동기화한 최신 정책 위에 한 번 더 저장하고, 실패는 원인 문장으로 알린다", async () => {
  const client = installMockClient();
  const mission = fakeMission({ revision: "5" });
  seedStore({ missions: [mission] });
  const original = useMissionStore.getState().syncMission;
  const sync = vi.fn(async () => {
    useMissionStore.setState((s) => ({ missions: { ...s.missions,
      [mission.id]: { ...mission, revision: "8", policy: { ...mission.policy, allow_network: true } } } }));
  });
  useMissionStore.setState({ syncMission: sync });
  const update = vi.spyOn(client, "missionPolicyUpdate")
    .mockRejectedValueOnce(new RpcClientError("REVISION_CONFLICT", "moved"))
    .mockResolvedValueOnce(result(mission.id))
    .mockRejectedValueOnce(new RpcClientError("INVALID_ARGUMENT", "the reduced policy conflicts with an owned run"));
  const ui = renderUi(<PolicyLimitsEditor mission={mission} />);
  try {
    setValue(ui.container.querySelector<HTMLInputElement>("[data-testid=limits-repair]")!, "2");
    click(ui.container.querySelector("[data-testid=limits-save]")!);
    await flushAsync();
    expect(sync).toHaveBeenCalledTimes(1);
    expect(update.mock.calls.map((call) => call[0].expected_revision)).toEqual(["5", "8"]);
    expect(update.mock.calls[1][0].policy).toMatchObject({ max_repair_cycles: 2, allow_network: true });
    setValue(ui.container.querySelector<HTMLInputElement>("[data-testid=limits-parallel]")!, "1");
    click(ui.container.querySelector("[data-testid=limits-save]")!);
    await flushAsync();
    const alert = ui.container.querySelector("[role=alert]")!;
    expect(alert.querySelector("[data-testid=mission-error-message]")?.textContent).toBe(t("missions.error.code.invalidArgument"));
    expect(alert.querySelector("details")?.textContent).toContain("INVALID_ARGUMENT: the reduced policy conflicts with an owned run");
  } finally {
    ui.unmount();
    useMissionStore.setState({ syncMission: original });
  }
});
