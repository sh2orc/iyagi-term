import { compatibleBinding } from "./testSupport";
import { beforeEach, expect, it, vi } from "vitest";
import { act } from "react";
import { CostPolicyEditor } from "./CostPolicyEditor";
import { UsageSummary } from "./UsageSummary";
import { RunDetail } from "./RunDetail";
import { MissionSettings } from "./MissionSettings";
import { newBinding } from "./configuration";
import { useMissionStore } from "./store";
import { useI18nStore, t } from "../../i18n";
import { click, fakeMission, fakeRun, fakeTask, flushAsync, installMockClient, renderUi, resetAllMissionState, seedStore, setValue } from "./testSupport";

beforeEach(resetAllMissionState);

it.each(["ko","en"] as const)("%s 사용량은 미확인 값을 0으로 합산하지 않고 정수 비용을 보존한다", language=>{
  useI18nStore.setState({language});
  const mission=fakeMission(),task=fakeTask(mission.id,"running");
  const unpriced=fakeRun(mission.id,task,"unknown",{binding_snapshot:newBinding(),usage:{input_tokens:null,output_tokens:null,cost_usd_micros:null,cost_source:"unknown"}});
  const reported=fakeRun(mission.id,task,"succeeded",{binding_snapshot:newBinding(),usage:{input_tokens:"9007199254740993",output_tokens:"2",cost_usd_micros:"9007199254740993",cost_source:"provider"}});
  const reserved=fakeRun(mission.id,task,"running",{binding_snapshot:{...newBinding(),estimated_run_cost_usd_micros:"600000"},usage:{input_tokens:"1",output_tokens:null,cost_usd_micros:"200000",cost_source:"provider"}});
  const ui=renderUi(<UsageSummary runs={[unpriced,reported,reserved]}/>);
  expect(ui.container.textContent).toContain("$9007199254.940993");
  expect(ui.container.textContent).toContain("$0.400000");
  expect(ui.container.textContent).toContain("9,007,199,254,740,994");
  expect(ui.container.textContent).toContain(t("missions.cost.partial",{known:"2",count:2}));
  expect(ui.container.textContent).toContain(t("missions.cost.estimates",{amount:"$0.400000",count:1}));
  ui.unmount();
});

it("비용 정책 저장은 최신 revision과 다른 정책 필드를 보존한다",async()=>{
  const client=installMockClient();const mission=fakeMission();seedStore({missions:[mission]});
  const save=vi.spyOn(client,"missionPolicyUpdate").mockResolvedValue({mission_id:mission.id,revision:"10",event_seq:"10",entity_ids:[]});
  const ui=renderUi(<CostPolicyEditor mission={mission}/>);
  setValue(ui.container.querySelector("input")!,"1.000001");
  setValue(ui.container.querySelector("select")!,"allow_with_notice");
  act(()=>useMissionStore.setState(s=>({missions:{...s.missions,[mission.id]:{...mission,revision:"9",policy:{...mission.policy,max_parallel_runs:2}}}})));
  click(ui.container.querySelector("button")!);await flushAsync();
  expect(save.mock.calls[0][0]).toMatchObject({expected_revision:"9",policy:{max_cost_usd_micros:"1000001",unknown_cost:"allow_with_notice",max_parallel_runs:2},role_bindings:mission.role_bindings});
  expect(ui.container.querySelector("[role=status]")?.textContent).toBe(t("missions.cost.policySaved"));
  setValue(ui.container.querySelector("input")!,"0");click(ui.container.querySelector("button")!);await flushAsync();
  expect(save).toHaveBeenCalledTimes(1);
  expect(ui.container.querySelector("[role=alert]")?.textContent).toBe(t("missions.create.costInvalid"));
  ui.unmount();
});

it.each(["cost_limit", "provider_rate_limited"])("%s 대기 작업의 모델 변경은 실패 재시도를 만들지 않는다",async blockedCode=>{
  const client=installMockClient();const mission=fakeMission();const binding=compatibleBinding();binding.label="Affordable";mission.policy.allowed_binding_ids=[binding.id];
  const task=fakeTask(mission.id,"blocked",{blocked_code:blockedCode,active_run_id:null});
  seedStore({missions:[mission],tasks:[task]});client.bindingList=vi.fn(async()=>({bindings:[binding]}));
  const save=vi.spyOn(client,"missionTaskControl").mockResolvedValue({mission_id:mission.id,revision:"6",event_seq:"6",entity_ids:[task.id]});
  const ui=renderUi(<RunDetail mission={mission} task={task} detailTab="exec" onDetailTabChange={()=>{}}/>);
  expect(ui.container.querySelector("[data-testid=task-retry]")).toBeNull();
  click(ui.container.querySelector("[data-testid=task-reassign]")!);await flushAsync();
  click(ui.container.querySelector("[data-testid=reassign-picker] button")!);await flushAsync();
  expect(save.mock.calls[0][0]).toMatchObject({action:"reassign",binding_id:binding.id,task_id:task.id});
  ui.unmount();
});

it("모델의 예상 비용을 정수 micros로 저장하고 비우면 미확인으로 저장한다",async()=>{
  const client=installMockClient();const save=vi.spyOn(client,"bindingSave");
  const ui=renderUi(<MissionSettings client={client}/>);await flushAsync();
  const field=(key:string)=>[...ui.container.querySelectorAll("label")].find(e=>e.firstChild?.textContent===t(key))!.querySelector("input,select") as HTMLInputElement;
  setValue(field("missions.settings.label"),"Budget model");setValue(field("missions.settings.program"),"/fixture/codex");
  // 감지된 카탈로그 때문에 모델 칸은 select다 — 카탈로그에 없는 id는 '직접 입력'으로.
  setValue(field("missions.settings.model"),"__manual__");
  setValue(ui.container.querySelector(`input[aria-label="${t("missions.settings.modelCustomInput")}"]`)!,"test-model");
  setValue(field("missions.settings.estimatedCost"),"0.125001");
  const button=[...ui.container.querySelectorAll("button")].find(b=>b.textContent===t("missions.settings.saveModel"))!;
  click(button);await flushAsync();expect(save.mock.calls[0][0].binding.estimated_run_cost_usd_micros).toBe("125001");
  setValue(field("missions.settings.estimatedCost"),"");click(button);await flushAsync();
  expect(save.mock.calls[1][0].binding.estimated_run_cost_usd_micros).toBeNull();ui.unmount();
});
