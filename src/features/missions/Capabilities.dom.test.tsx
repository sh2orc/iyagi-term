import { beforeEach, expect, it, vi } from "vitest";
import { useI18nStore, t } from "../../i18n";
import { newBinding } from "./configuration";
import { bindingSupportsRole } from "./bindingSupport";
import { CapabilityNotice } from "./CapabilityNotice";
import { RunDetail } from "./RunDetail";
import { click, compatibleBinding, fakeMission, fakeRun, fakeTask, flushAsync,
  installMockClient, renderUi, resetAllMissionState, seedStore } from "./testSupport";

beforeEach(resetAllMissionState);

it.each(["ko", "en"] as const)("%s 호환성 대기는 검증된 모델로만 재배정한다", async language => {
  useI18nStore.setState({language});
  const client = installMockClient();
  const mission = fakeMission();
  const unsupported = {...newBinding(), label: "Unverified"};
  const supported = {...compatibleBinding(), label: "Verified"};
  mission.policy.allowed_binding_ids = [unsupported.id, supported.id];
  const task = fakeTask(mission.id, "blocked", {blocked_code:"capability_scoped_write", active_run_id:null});
  seedStore({missions:[mission], tasks:[task]});
  client.bindingList = vi.fn(async () => ({bindings:[unsupported, supported]}));
  const save = vi.spyOn(client, "missionTaskControl").mockResolvedValue({mission_id:mission.id, revision:"2", event_seq:"2", entity_ids:[task.id]});
  const ui = renderUi(<RunDetail mission={mission} task={task} detailTab="exec" onDetailTabChange={()=>{}}/>);
  expect(ui.container.querySelector("[data-testid=capability-notice]")?.textContent).toContain(t("missions.compatibility.waiting"));
  click(ui.container.querySelector("[data-testid=task-reassign]")!); await flushAsync();
  const buttons = [...ui.container.querySelectorAll<HTMLButtonElement>("[data-testid=reassign-picker] button")];
  const invalid = buttons.find(b=>b.textContent?.startsWith("Unverified"))!;
  expect(invalid.disabled).toBe(true);
  expect(invalid.title).toBe(t("missions.compatibility.unsupported"));
  click(invalid); expect(save).not.toHaveBeenCalled();
  click(buttons.find(b=>b.textContent?.startsWith("Verified"))!); await flushAsync();
  expect(save.mock.calls[0][0]).toMatchObject({action:"reassign", binding_id:supported.id});
  ui.unmount();
});

it("쓰기 지원과 읽기 지원을 역할별로 구분하고 선택 기능은 요구하지 않는다", () => {
  const binding = compatibleBinding();
  binding.capabilities.scoped_write.supported = false;
  for (const role of ["builder", "test_author", "documenter", "integrator"] as const) expect(bindingSupportsRole(binding, role)).toBe(false);
  for (const role of ["lead", "researcher", "architect", "reviewer", "specialist", "diagnostician"] as const) expect(bindingSupportsRole(binding, role)).toBe(true);
  binding.capabilities.steer.supported = false;
  binding.capabilities.resume.supported = false;
  expect(bindingSupportsRole(binding, "lead")).toBe(true);
  binding.capabilities.cancel.supported = false;
  expect(bindingSupportsRole(binding, "lead")).toBe(false);
});

it("미전송 증거가 있을 때만 실행 전 실패라고 안내한다", () => {
  const mission = fakeMission(), task = fakeTask(mission.id, "failed");
  const run = fakeRun(mission.id, task, "failed", {failure_code:"CAPABILITY_UNSUPPORTED"});
  let ui = renderUi(<CapabilityNotice task={task} run={run}/>);
  expect(ui.container.textContent).toBe(""); ui.unmount();
  ui = renderUi(<CapabilityNotice task={task} run={{...run, retry_evidence:{basis:"request_not_submitted",observed_at_unix_ms:"1",retry_after_unix_ms:null}}}/>);
  expect(ui.container.textContent).toContain(t("missions.compatibility.changed")); ui.unmount();
});
