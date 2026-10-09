/**
 * MissionTeamEditor: 역할별 모델 연결 교체, 필수 역할 보호, 지원하지 않는 연결 비활성,
 * 허용 목록 합집합 저장, 편집할 수 없는 상태 안내.
 */

import { beforeEach, expect, it, vi } from "vitest";
import { t } from "../../i18n";
import { newBinding } from "./configuration";
import { MissionTeamEditor } from "./MissionTeamEditor";
import {
  click, compatibleBinding, fakeMission, flushAsync, installMockClient, renderUi,
  resetAllMissionState, seedStore, setValue,
} from "./testSupport";

beforeEach(resetAllMissionState);

const result = (missionId: string) => ({ mission_id: missionId, revision: "6", event_seq: "6", entity_ids: [] });

/** lead·builder·reviewer·integrator를 모두 허용하고 lead·builder만 배정한 작업. */
function teamRig(state: "running" | "completed" = "running") {
  const client = installMockClient();
  const lead = { ...compatibleBinding(), label: "Lead model" };
  const other = { ...compatibleBinding(), label: "Other model" };
  const unsupported = { ...newBinding(), label: "Unverified" };
  const mission = fakeMission({ state });
  mission.policy.allowed_roles = ["lead", "builder", "reviewer", "integrator"];
  mission.policy.allowed_binding_ids = [lead.id];
  mission.policy.require_independent_review = false;
  mission.role_bindings = [
    { role: "lead", primary_binding_id: lead.id, fallback_binding_ids: [] },
    { role: "builder", primary_binding_id: lead.id, fallback_binding_ids: [] },
  ];
  seedStore({ missions: [mission] });
  client.bindingList = vi.fn(async () => ({ bindings: [lead, other, unsupported] }));
  return { client, mission, lead, other, unsupported };
}

it("필수 역할은 비울 수 없고 선택 역할만 `맡기지 않음`을 가진다", async () => {
  const { mission } = teamRig();
  const ui = renderUi(<MissionTeamEditor mission={mission} />);
  await flushAsync();
  const options = (role: string) =>
    [...ui.container.querySelectorAll<HTMLOptionElement>(`[data-testid=team-pick-${role}] option`)].map((o) => o.textContent);
  expect(options("lead")).not.toContain(t("missions.team.unassigned"));
  expect(options("builder")).not.toContain(t("missions.team.unassigned"));
  // 독립 리뷰가 꺼져 있으면 reviewer도 선택 역할이다.
  expect(options("reviewer")).toContain(t("missions.team.unassigned"));
  expect(options("integrator")).toContain(t("missions.team.unassigned"));
  expect(ui.container.querySelector<HTMLButtonElement>("[data-testid=team-save]")!.disabled).toBe(true);
  ui.unmount();
});

it("독립 리뷰가 켜져 있으면 reviewer도 필수이고, 비어 있으면 저장을 막는다", async () => {
  const { mission } = teamRig();
  const reviewed = { ...mission, policy: { ...mission.policy, require_independent_review: true } };
  seedStore({ missions: [reviewed] });
  const ui = renderUi(<MissionTeamEditor mission={reviewed} />);
  await flushAsync();
  const options = [...ui.container.querySelectorAll<HTMLOptionElement>("[data-testid=team-pick-reviewer] option")];
  expect(options.map((o) => o.textContent)).not.toContain(t("missions.team.unassigned"));
  expect(ui.container.querySelector("[data-testid=team-missing-required]")!.textContent)
    .toBe(t("missions.team.missingRequired", { roles: t("missions.role.reviewer") }));
  expect(ui.container.querySelector<HTMLButtonElement>("[data-testid=team-save]")!.disabled).toBe(true);
  ui.unmount();
});

it("역할이 맡는 할 일을 지원하지 않는 연결은 고를 수 없다", async () => {
  const { mission, unsupported } = teamRig();
  const ui = renderUi(<MissionTeamEditor mission={mission} />);
  await flushAsync();
  const option = [...ui.container.querySelectorAll<HTMLOptionElement>("[data-testid=team-pick-lead] option")]
    .find((o) => o.value === unsupported.id)!;
  expect(option.disabled).toBe(true);
  expect(option.textContent).toBe(t("missions.team.unsupportedOption", { model: "Unverified" }));
  ui.unmount();
});

it("모델을 바꾸면 새 연결을 허용 목록에 더해 policy.update로 저장한다", async () => {
  const { client, mission, lead, other } = teamRig();
  const update = vi.spyOn(client, "missionPolicyUpdate").mockResolvedValue(result(mission.id));
  const ui = renderUi(<MissionTeamEditor mission={mission} onClose={() => undefined} />);
  await flushAsync();
  setValue(ui.container.querySelector<HTMLSelectElement>("[data-testid=team-pick-builder]")!, other.id);
  setValue(ui.container.querySelector<HTMLSelectElement>("[data-testid=team-pick-integrator]")!, other.id);
  click(ui.container.querySelector("[data-testid=team-save]")!);
  await flushAsync();
  expect(update).toHaveBeenCalledTimes(1);
  const params = update.mock.calls[0][0];
  expect(params.expected_revision).toBe(mission.revision);
  expect(params.role_bindings).toEqual([
    { role: "lead", primary_binding_id: lead.id, fallback_binding_ids: [] },
    { role: "builder", primary_binding_id: other.id, fallback_binding_ids: [] },
    { role: "integrator", primary_binding_id: other.id, fallback_binding_ids: [] },
  ]);
  // 허용 목록은 더하기만 한다 — 실행 중인 Run이 쓰는 연결을 지우지 않는다.
  expect([...params.policy.allowed_binding_ids].sort()).toEqual([lead.id, other.id].sort());
  expect(ui.container.querySelector("[data-testid=team-saved]")!.textContent).toBe(t("missions.team.saved"));
  ui.unmount();
});

it("선택 역할을 비우면 역할 연결에서 빠진다", async () => {
  const { client, mission, lead, other } = teamRig();
  const withIntegrator = {
    ...mission,
    role_bindings: [...mission.role_bindings, { role: "integrator" as const, primary_binding_id: other.id, fallback_binding_ids: [] }],
  };
  seedStore({ missions: [withIntegrator] });
  const update = vi.spyOn(client, "missionPolicyUpdate").mockResolvedValue(result(mission.id));
  const ui = renderUi(<MissionTeamEditor mission={withIntegrator} />);
  await flushAsync();
  setValue(ui.container.querySelector<HTMLSelectElement>("[data-testid=team-pick-integrator]")!, "");
  click(ui.container.querySelector("[data-testid=team-save]")!);
  await flushAsync();
  expect(update.mock.calls[0][0].role_bindings.map((binding) => binding.role)).toEqual(["lead", "builder"]);
  expect(update.mock.calls[0][0].policy.allowed_binding_ids).toContain(lead.id);
  ui.unmount();
});

it("끝난 작업은 팀을 바꿀 수 없다고 알리고 선택을 막는다", async () => {
  const { mission } = teamRig("completed");
  const ui = renderUi(<MissionTeamEditor mission={mission} />);
  await flushAsync();
  expect(ui.container.querySelector("[data-testid=team-not-editable]")!.textContent).toBe(t("missions.team.notEditable"));
  expect(ui.container.querySelector<HTMLSelectElement>("[data-testid=team-pick-lead]")!.disabled).toBe(true);
  ui.unmount();
});
