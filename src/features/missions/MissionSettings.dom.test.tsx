import { beforeEach, expect, it, vi } from "vitest";
import type { DetectedRuntime } from "../../generated/DetectedRuntime";
import type { RuntimeDetectResult } from "../../generated/RuntimeDetectResult";
import type { MockDaemonClient } from "../daemon/mockClient";
import { MissionSettings } from "./MissionSettings";
import { CUSTOM_MODEL_OPTION, missionPolicy, newBinding } from "./configuration";
import { t, useI18nStore } from "../../i18n";
import { click, flushAsync, installMockClient, renderUi, resetAllMissionState, setValue } from "./testSupport";

beforeEach(resetAllMissionState);

/** Quick setup detects on mount; keep these cases independent of the host's installed CLIs. */
function stubDetect(client: MockDaemonClient, runtimes: DetectedRuntime[] = []): MockDaemonClient {
  client.runtimeDetect = vi.fn(async (): Promise<RuntimeDetectResult> => ({ runtimes }));
  return client;
}
function buttonIn(root: Element, key: string): HTMLButtonElement {
  const node = [...root.querySelectorAll("button")].find(candidate => candidate.textContent === t(key));
  if (!node) throw new Error(`Missing button: ${key}`);
  return node;
}
/** runtime.detect fixture whose interesting part is only the model list it advertises. */
function detectedRuntime(runtime: DetectedRuntime["runtime"], models: DetectedRuntime["models"]): DetectedRuntime {
  return {runtime,program:`/fixture/${runtime}`,version:"1.0.0",installation:"verified",login:"found",configured_model_id:null,
    suggested_provider_id:runtime==="claude"?"anthropic":"openai",proven_model_id:null,models,verified_roles:[],grade:"unverified",experimental_roles:[]};
}
/** Ids the model select offers, without the placeholder and the manual-entry switch; empty once the form has fallen back to a plain input. */
function modelChoicesIn(root: Element): string[] {
  const select=root.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.settings.model")}"]`);
  return select ? [...select.options].map(option=>option.value).filter(value=>value!=="" && value!==CUSTOM_MODEL_OPTION) : [];
}
/** The free-text model field: shown without a catalog, or after choosing manual entry. */
function manualModelIn(root: Element): HTMLInputElement | null {
  return root.querySelector<HTMLInputElement>(`input[aria-label="${t("missions.settings.modelCustomInput")}"]`);
}
/** The advanced connection form looks fields up by their label text. */
function fieldIn(root: Element, key: string): HTMLInputElement | HTMLSelectElement {
  const node=[...root.querySelectorAll("label")].find(label=>label.firstChild?.textContent===t(key))?.querySelector("input,select");
  if (!node) throw new Error(`Missing field: ${key}`);
  return node as HTMLInputElement | HTMLSelectElement;
}
/** Absent (not just empty) when nothing advertises an effort for the typed model. */
function effortSelectIn(root: Element): HTMLSelectElement | null {
  return root.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.settings.effort")}"]`);
}

it.each(["ko", "en"] as const)("%s 설치 실패를 활성화 설정과 구분하고 미저장 편집을 보호한다", async language => {
  useI18nStore.setState({ language });
  const client=stubDetect(installMockClient());
  const saved=(await client.bindingSave({request_id:crypto.randomUUID(),expected_revision:"0",
    binding:{...newBinding(),label:"Existing",program:"/fixture/cli",model_id:"model",enabled:true,runtime_version:"old-version"}})).binding;
  const probe=vi.spyOn(client,"bindingProbe").mockResolvedValue({binding:{...saved,revision:"2",runtime_version:null,checked_at:"2026-09-16T00:00:00Z"},models:[],installation:"timed_out"});
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    const field=(key:string)=>[...handle.container.querySelectorAll("label")].find(label=>label.firstChild?.textContent===t(key))!.querySelector("input,select") as HTMLInputElement | HTMLSelectElement;
    setValue(field("missions.settings.savedModels"),saved.id);
    await flushAsync();
    const button=[...handle.container.querySelectorAll("button")].find(button=>button.textContent===t("missions.settings.probeNow"))!;
    setValue(field("missions.settings.label"),"Unsaved edit");
    expect(button.disabled).toBe(true);
    expect(handle.container.textContent).toContain(t("missions.settings.probeSaveFirst"));
    expect(probe).not.toHaveBeenCalled();
    setValue(field("missions.settings.label"),saved.label);
    expect(button.disabled).toBe(false);
    click(button);await flushAsync();
    expect(handle.container.querySelector("[data-testid=mission-settings-notice]")?.textContent).toBe(t("missions.settings.probeStatus.timed_out"));
    expect(handle.container.textContent).not.toContain("old-version");
    expect(button.disabled).toBe(false);
    // The new probe revision is used by subsequent saves, without disabling
    // this connection just because the installation check failed.
    setValue(field("missions.settings.label"),"After probe");
    const save=vi.spyOn(client,"bindingSave");
    click([...handle.container.querySelectorAll("button")].find(button=>button.textContent===t("missions.settings.saveModel"))!);
    await flushAsync();
    expect(save.mock.calls[0][0]).toMatchObject({expected_revision:"2",binding:{enabled:true,label:"After probe"}});
  } finally {handle.unmount();}
});

it.each(["ko", "en"] as const)("%s 연결 확인이 돌려준 모델 목록으로 모델 입력의 후보를 채운다", async language => {
  useI18nStore.setState({ language });
  const client=stubDetect(installMockClient());
  const saved=(await client.bindingSave({request_id:crypto.randomUUID(),expected_revision:"0",
    binding:{...newBinding(),label:"Existing",program:"/fixture/cli",model_id:"model",enabled:true}})).binding;
  vi.spyOn(client,"bindingProbe").mockResolvedValue({binding:{...saved,revision:"2"},
    models:[{id:"gpt-6-astra",efforts:[]},{id:"gpt-5.6-sol",efforts:[]}],installation:"verified"});
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    const field=(key:string)=>[...handle.container.querySelectorAll("label")].find(label=>label.firstChild?.textContent===t(key))!.querySelector("input,select") as HTMLInputElement | HTMLSelectElement;
    setValue(field("missions.settings.savedModels"),saved.id);
    await flushAsync();
    // Before a probe runs, nothing has been advertised for this connection yet: the model stays a plain text field.
    expect(modelChoicesIn(handle.container)).toEqual([]);
    expect((field("missions.settings.model") as HTMLInputElement).value).toBe("model");
    const probeButton=[...handle.container.querySelectorAll("button")].find(button=>button.textContent===t("missions.settings.probeNow"))!;
    click(probeButton);await flushAsync();
    // What the connection answered becomes the select, with the stored id kept selectable ahead of it.
    const modelSelect=field("missions.settings.model") as HTMLSelectElement;
    expect(modelSelect.value).toBe("model");
    expect(modelChoicesIn(handle.container)).toEqual(["model","gpt-6-astra","gpt-5.6-sol"]);
    // Manual entry stays available for an id the catalog does not name, and the select stays on it while typing.
    setValue(modelSelect,CUSTOM_MODEL_OPTION);
    const manual=manualModelIn(handle.container)!;
    setValue(manual,"a-typed-model-not-in-the-list");
    expect(manual.value).toBe("a-typed-model-not-in-the-list");
    expect(modelSelect.value).toBe(CUSTOM_MODEL_OPTION);
  } finally {handle.unmount();}
});

it.each(["ko", "en"] as const)("%s 저장된 팀의 역할은 slug·키 원문이 아니라 사람 이름으로 보인다(test_author 포함)", async language => {
  useI18nStore.setState({ language });
  const client = stubDetect(installMockClient());
  const model = { ...newBinding(), label: "Writer model", model_id: "writer-id" };
  client.bindingList = vi.fn(async () => ({ bindings: [model] }));
  client.templateList = vi.fn(async () => ({ templates: [{ id: "team-1", revision: "1", label: "Test team", repository_id: null,
    role_bindings: (["lead", "test_author"] as const).map(role => ({ role, primary_binding_id: model.id, fallback_binding_ids: [] })),
    policy: missionPolicy([model.id]) }] }));
  const handle = renderUi(<MissionSettings client={client} />);
  await flushAsync();
  const card = handle.container.querySelector("[data-setting-id=missionTeams]")!;
  expect(card.textContent).toContain(`${t("missions.role.testAuthor")}: Writer model`);
  expect(card.textContent).not.toContain("missions.role.");
  expect(card.textContent).not.toContain("test_author");
  handle.unmount();
});

it.each(["ko", "en"] as const)("%s 팀의 Integrator 기본 배정과 명시적 변경을 저장해 표시한다", async language => {
  useI18nStore.setState({ language });
  const client = stubDetect(installMockClient());
  const builder = { ...newBinding(), label: "Builder model", model_id: "builder-id" };
  const integrator = { ...newBinding(), label: "Conflict model", model_id: "integrator-id" };
  client.bindingList = vi.fn(async () => ({ bindings: [builder, integrator] }));
  const save = vi.spyOn(client, "templateSave");
  const handle = renderUi(<MissionSettings client={client} />);
  await flushAsync();
  const card = handle.container.querySelector("[data-setting-id=missionTeams]")!;
  const field = (role: string) => card.querySelector<HTMLSelectElement>(`select[aria-label="${t(`missions.role.${role}`)}"]`)!;
  setValue(card.querySelector("input")!, "Default integrator");
  for (const role of ["lead", "builder", "reviewer"]) setValue(field(role), builder.id);
  expect((field("integrator") as HTMLSelectElement).value).toBe(builder.id);
  click(card.querySelector("button")!); await flushAsync();
  expect(save.mock.calls[0][0].template.role_bindings).toContainEqual({ role: "integrator", primary_binding_id: builder.id, fallback_binding_ids: [] });
  expect(save.mock.calls[0][0].template.policy.allowed_roles).toContain("integrator");
  setValue(card.querySelector("input")!, "Explicit integrator");
  setValue(field("integrator"), integrator.id);
  click(card.querySelector("button")!); await flushAsync();
  expect(save.mock.calls[1][0].template.role_bindings).toContainEqual({ role: "integrator", primary_binding_id: integrator.id, fallback_binding_ids: [] });
  expect(card.textContent).toContain(`${t("missions.role.integrator")}: Conflict model`);
  handle.unmount();
});

it("rejects a raw key before RPC, then saves only the connection references", async () => {
  const client=stubDetect(installMockClient());
  const save=vi.spyOn(client,"bindingSave");
  const handle=renderUi(<MissionSettings client={client}/>);
  const field=(label:string)=>{
    const node=[...handle.container.querySelectorAll("label")].find(node=>node.firstChild?.textContent===label);
    const input=node?.querySelector("input,select");
    if (!input) throw new Error(`Missing field: ${label}`);
    return input as HTMLInputElement | HTMLSelectElement;
  };
  try {
    await flushAsync();
    setValue(field("CLI 종류"),"opencode");
    setValue(field("표시 이름"),"Z.ai Coding");
    setValue(field("CLI 실행 파일의 전체 경로"),"/fixture/opencode");
    setValue(field("모델 ID"),"glm-5.3");
    setValue(field("제공자 ID"),"zai-coding-plan");
    setValue(field("인증 방식"),"subscription");
    setValue(field("저장된 인증 참조"),"fake-key-accidentally-pasted");
    const button=[...handle.container.querySelectorAll("button")].find(button=>button.textContent==="모델 저장")!;
    click(button); await flushAsync();
    expect(save).not.toHaveBeenCalled();
    expect(handle.container.querySelector("[data-testid=mission-settings-error]")?.textContent).toContain("keyring:UUID");
    const id="11111111-1111-4111-8111-111111111111";
    setValue(field("저장된 인증 참조"),`keyring:${id}`);
    setValue(field("저장된 엔드포인트 ID"),id);
    click(button); await flushAsync();
    expect(save).toHaveBeenCalledTimes(1);
    expect(save.mock.calls[0][0].binding).toMatchObject({runtime:"opencode",credential_ref:`keyring:${id}`,endpoint_ref:id,model_id:"glm-5.3",provider_id:"zai-coding-plan",auth_route:"subscription"});
    expect(JSON.stringify(save.mock.calls)).not.toContain("fake-key-accidentally-pasted");
  } finally { handle.unmount(); }
});

it("saves Codex API references and clears them when switching to managed subscription", async () => {
  const client=stubDetect(installMockClient());
  const save=vi.spyOn(client,"bindingSave");
  const handle=renderUi(<MissionSettings client={client}/>);
  const field=(label:string)=>{
    const node=[...handle.container.querySelectorAll("label")].find(node=>node.firstChild?.textContent===label);
    const input=node?.querySelector("input,select");
    if (!input) throw new Error(`Missing field: ${label}`);
    return input as HTMLInputElement | HTMLSelectElement;
  };
  try {
    await flushAsync();
    expect(handle.container.textContent).toContain("CODEX_HOME");
    setValue(field("표시 이름"),"Codex API");
    setValue(field("CLI 실행 파일의 전체 경로"),"/fixture/codex");
    setValue(field("모델 ID"),"exact-model-id");
    setValue(field("인증 방식"),"api_key");
    expect(handle.container.textContent).toContain("--preset codex-api");
    const id="11111111-1111-4111-8111-111111111111";
    setValue(field("저장된 인증 참조"),`keyring:${id}`);
    setValue(field("저장된 엔드포인트 ID"),id);
    const button=[...handle.container.querySelectorAll("button")].find(button=>button.textContent==="모델 저장")!;
    click(button); await flushAsync();
    expect(save.mock.calls[0][0].binding).toMatchObject({runtime:"codex",provider_id:"openai",auth_route:"api_key",credential_ref:`keyring:${id}`,endpoint_ref:id,model_id:"exact-model-id"});
    setValue(field("인증 방식"),"subscription");
    for (const label of ["저장된 인증 참조", "저장된 엔드포인트 ID"]) {
      expect([...handle.container.querySelectorAll("label")].some(node => node.firstChild?.textContent === label)).toBe(false);
    }
    click(button); await flushAsync();
    expect(save.mock.calls[1][0].binding).toMatchObject({runtime:"codex",auth_route:"subscription",credential_ref:null,endpoint_ref:null});
  } finally {handle.unmount();}
});

it("saves Claude subscription-token references and permits managed login with both references cleared", async () => {
  const client=stubDetect(installMockClient());const save=vi.spyOn(client,"bindingSave");
  const handle=renderUi(<MissionSettings client={client}/>);
  const field=(label:string)=>[...handle.container.querySelectorAll("label")].find(node=>node.firstChild?.textContent===label)!.querySelector("input,select") as HTMLInputElement | HTMLSelectElement;
  try {
    await flushAsync();
    setValue(field("CLI 종류"),"claude");
    setValue(field("표시 이름"),"Claude subscription");
    setValue(field("CLI 실행 파일의 전체 경로"),"/fixture/claude");
    setValue(field("모델 ID"),"exact-claude-model");
    expect(handle.container.textContent).toContain("claude-subscription");
    const id="11111111-1111-4111-8111-111111111111";
    setValue(field("저장된 인증 참조"),`keyring:${id}`);setValue(field("저장된 엔드포인트 ID"),id);
    const button=[...handle.container.querySelectorAll("button")].find(button=>button.textContent==="모델 저장")!;
    click(button);await flushAsync();
    expect(save.mock.calls[0][0].binding).toMatchObject({runtime:"claude",provider_id:"anthropic",auth_route:"subscription",credential_ref:`keyring:${id}`,endpoint_ref:id});
    setValue(field("저장된 인증 참조"),"");setValue(field("저장된 엔드포인트 ID"),"");
    click(button);await flushAsync();
    expect(save.mock.calls[1][0].binding).toMatchObject({runtime:"claude",auth_route:"subscription",credential_ref:null,endpoint_ref:null});
  } finally {handle.unmount();}
});

it("puts quick setup first and keeps the manual connection form collapsed under the searchable section", async () => {
  const client=stubDetect(installMockClient());
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    expect(client.runtimeDetect).toHaveBeenCalledTimes(1);
    const root=handle.container.querySelector(".mission-settings")!;
    const quick=root.querySelector("[data-testid=mission-quick-setup]")!;
    expect(root.querySelector("h2")!.nextElementSibling).toBe(quick);
    expect(quick.textContent).toContain(t("missions.quickSetup.title"));
    const guide=root.querySelector("[data-testid=mission-guide]")!;
    expect(quick.compareDocumentPosition(guide)&Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    const models=root.querySelector("[data-setting-id=missionModels]")!;
    const advanced=models.querySelector("details[data-testid=mission-model-advanced]")!;
    expect(advanced.hasAttribute("open")).toBe(false);
    expect(advanced.querySelector("summary")?.textContent).toBe(t("missions.settings.advanced"));
    for (const key of ["missions.settings.savedModels","missions.settings.label","missions.settings.runtime","missions.settings.program","missions.settings.model"]) {
      expect([...advanced.querySelectorAll("label")].some(label=>label.firstChild?.textContent===t(key))).toBe(true);
    }
    expect(advanced.contains(buttonIn(models,"missions.settings.saveModel"))).toBe(true);
    expect(advanced.contains(buttonIn(models,"missions.settings.probeNow"))).toBe(true);
  } finally { handle.unmount(); }
});

it.each(["ko", "en"] as const)("%s 모든 역할에 같은 모델을 한 번에 지정하고 개별 변경을 유지한다", async language => {
  useI18nStore.setState({ language });
  const client=stubDetect(installMockClient());
  const first={...newBinding(),label:"First model",model_id:"first-id"};
  const second={...newBinding(),label:"Second model",model_id:"second-id"};
  client.bindingList=vi.fn(async()=>({bindings:[first,second]}));
  const save=vi.spyOn(client,"templateSave");
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    const card=handle.container.querySelector("[data-setting-id=missionTeams]")!;
    const all=card.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.settings.allRoles")}"]`)!;
    const role=(name:string)=>card.querySelector<HTMLSelectElement>(`select[aria-label="${t(`missions.role.${name}`)}"]`)!;
    setValue(all,first.id);
    for (const name of ["lead","builder","reviewer","integrator"]) expect(role(name).value).toBe(first.id);
    expect(all.value).toBe(first.id);
    setValue(role("reviewer"),second.id);
    expect(all.value).toBe("");
    expect(role("lead").value).toBe(first.id);
    setValue(card.querySelector("input")!,"Shared model team");
    click(buttonIn(card,"missions.settings.saveTeam"));await flushAsync();
    expect(save.mock.calls[0][0].template.role_bindings).toEqual([
      {role:"lead",primary_binding_id:first.id,fallback_binding_ids:[]},
      {role:"builder",primary_binding_id:first.id,fallback_binding_ids:[]},
      {role:"reviewer",primary_binding_id:second.id,fallback_binding_ids:[]},
      {role:"integrator",primary_binding_id:first.id,fallback_binding_ids:[]},
    ]);
    expect(save.mock.calls[0][0].template.policy.allowed_binding_ids).toEqual([first.id,second.id]);
    setValue(all,first.id);
    setValue(all,"");
    for (const name of ["lead","builder","reviewer","integrator"]) expect(role(name).value).toBe("");
  } finally { handle.unmount(); }
});

it.each(["ko", "en"] as const)("%s 검증 명령 인수를 공백 구분이나 JSON 배열로 저장하고 짝이 맞지 않는 따옴표를 거부한다", async language => {
  useI18nStore.setState({ language });
  const client=stubDetect(installMockClient());
  const save=vi.spyOn(client,"verificationSave");
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    const card=handle.container.querySelector("[data-setting-id=missionVerification]")!;
    const field=(key:string)=>[...card.querySelectorAll("label")].find(label=>label.firstChild?.textContent===t(key))!.querySelector("input") as HTMLInputElement;
    const args=field("missions.settings.commandArgs");
    expect(args.value).toBe("");
    expect(args.placeholder).toBe("test -- --run");
    setValue(field("missions.create.repo"),"/repo");
    click(buttonIn(card,"missions.create.inspect"));await flushAsync();
    setValue(field("missions.settings.commandTitle"),"Unit tests");
    setValue(field("missions.settings.commandProgram"),"npm");
    setValue(args,'test -- --run "src/a b.test.ts"');
    click(buttonIn(card,"missions.settings.saveCommand"));await flushAsync();
    expect(save.mock.calls[0][0].command.argv).toEqual(["test","--","--run","src/a b.test.ts"]);
    setValue(field("missions.settings.commandTitle"),"Lint");
    setValue(args,'["run", "lint"]');
    click(buttonIn(card,"missions.settings.saveCommand"));await flushAsync();
    expect(save.mock.calls[1][0].command.argv).toEqual(["run","lint"]);
    setValue(field("missions.settings.commandTitle"),"No arguments");
    setValue(args,"   ");
    click(buttonIn(card,"missions.settings.saveCommand"));await flushAsync();
    expect(save.mock.calls[2][0].command.argv).toEqual([]);
    setValue(field("missions.settings.commandTitle"),"Broken");
    setValue(args,'test "unterminated');
    click(buttonIn(card,"missions.settings.saveCommand"));await flushAsync();
    expect(save).toHaveBeenCalledTimes(3);
    expect(handle.container.querySelector("[data-testid=mission-settings-error]")?.textContent).toBe(t("missions.settings.argsInvalid"));
  } finally { handle.unmount(); }
});

it("shows the connection and team created by quick setup in the sections below", async () => {
  // The mock probe verifies this Codex subscription + gpt-5.6-luna combination like the daemon registry.
  const client=stubDetect(installMockClient(),[{runtime:"codex",program:"/usr/local/bin/codex",version:"0.154.0",installation:"verified",login:"found",
    configured_model_id:null,suggested_provider_id:"openai",proven_model_id:"gpt-5.6-luna",models:[],verified_roles:["lead","builder","reviewer","integrator"],grade:"verified",experimental_roles:[]}]);
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    const teams=handle.container.querySelector("[data-setting-id=missionTeams]")!;
    expect(teams.textContent).not.toContain("Codex · gpt-5.6-luna");
    const quick=handle.container.querySelector("[data-testid=mission-quick-setup]")!;
    click(buttonIn(quick,"missions.quickSetup.create"));await flushAsync();
    expect(quick.querySelector("[role=status]")?.textContent).toBe(t("missions.quickSetup.done",{label:"Codex · gpt-5.6-luna"}));
    expect(teams.querySelector("ul")?.textContent).toContain("Codex · gpt-5.6-luna");
    expect(teams.textContent).toContain(`${t("missions.role.integrator")}: Codex · gpt-5.6-luna`);
    const lead=teams.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.role.lead")}"]`)!;
    expect([...lead.options].map(option=>option.textContent)).toContain("Codex · gpt-5.6-luna · gpt-5.6-luna");
    const saved=handle.container.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.settings.savedModels")}"]`)!;
    expect([...saved.options].map(option=>option.textContent)).toContain("Codex · gpt-5.6-luna · gpt-5.6-luna");
  } finally { handle.unmount(); }
});

it("lists the connection quick setup saved even when setup stops before creating a team", async () => {
  // Graded verified for this CLI, but here the self-check proves nothing for the connection: setup
  // stops after saving and probing it.
  const client=stubDetect(installMockClient(),[{runtime:"codex",program:"/usr/local/bin/codex",version:"0.160.0",installation:"verified",login:"found",
    configured_model_id:"other-model",suggested_provider_id:"openai",proven_model_id:null,models:[],verified_roles:[],grade:"verified",experimental_roles:[]}]);
  const probed=client.bindingProbe.bind(client);
  vi.spyOn(client,"bindingProbe").mockImplementation(async params=>{
    const result=await probed(params);
    const unknown={supported:false,reason_code:"no_compatibility_evidence"};
    return {...result,binding:{...result.binding,capabilities:{...result.binding.capabilities,
      structured_result:unknown,events:unknown,cancel:unknown,read_only:unknown,scoped_write:unknown}}};
  });
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    const list=vi.spyOn(client,"bindingList");
    const templateSave=vi.spyOn(client,"templateSave");
    const quick=handle.container.querySelector("[data-testid=mission-quick-setup]")!;
    click(buttonIn(quick,"missions.quickSetup.create"));await flushAsync();
    expect(quick.querySelector("[role=alert]")?.textContent).toBe(t("missions.quickSetup.rolesBlocked",{roles:"Lead, Builder, Reviewer, Integrator"}));
    expect(templateSave).not.toHaveBeenCalled();
    // Only quick setup's own lookup ran — the sections below were updated from its callback, not a reload.
    expect(list).toHaveBeenCalledTimes(1);
    const saved=handle.container.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.settings.savedModels")}"]`)!;
    const option=[...saved.options].find(candidate=>candidate.textContent==="Codex · other-model · other-model");
    expect(option).toBeDefined();
    // The listed copy is the probed revision: selecting and saving it does not hit a revision conflict.
    setValue(saved,option!.value);
    await flushAsync();
    expect(handle.container.textContent).not.toContain(t("missions.settings.probeSaveFirst"));
    expect(buttonIn(handle.container,"missions.settings.probeNow").disabled).toBe(false);
    const save=vi.spyOn(client,"bindingSave");
    click(buttonIn(handle.container,"missions.settings.saveModel"));await flushAsync();
    expect(save.mock.calls[0][0].expected_revision).toBe("2");
    expect(handle.container.querySelector("[data-testid=mission-settings-error]")).toBeNull();
    expect(handle.container.querySelector("[data-testid=mission-settings-notice]")?.textContent).toBe(t("missions.settings.saved"));
  } finally { handle.unmount(); }
});

it.each(["ko", "en"] as const)("%s 실험적 연결은 신뢰 칩으로 알리고, CLI가 업데이트돼도 다시 동의를 묻지 않는다", async language => {
  useI18nStore.setState({ language });
  const client=stubDetect(installMockClient());
  const experimental={supported:true,reason_code:"experimental_opt_in"};
  const base=newBinding("claude");
  const saved=(await client.bindingSave({request_id:crypto.randomUUID(),expected_revision:"0",binding:{...base,label:"Claude exp",program:"/fixture/claude",model_id:"claude-opus-5",
    runtime_version:"2.1.300",experimental_version:"2.1.300",capabilities:{...base.capabilities,structured_result:experimental,events:experimental,cancel:experimental,read_only:experimental,scoped_write:experimental}}})).binding;
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    const select=handle.container.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.settings.savedModels")}"]`)!;
    setValue(select,saved.id);
    await flushAsync();
    // 칩 하나가 증거 층을 말한다: 필수 기능이 동의로 열렸으므로 `실험적`, 옆에 동의한 버전.
    const chip=handle.container.querySelector("[data-testid=mission-binding-trust]")!;
    expect(chip.getAttribute("data-trust")).toBe("experimental");
    expect(chip.textContent).toContain(t("missions.trust.experimental"));
    expect(chip.textContent).toContain(t("missions.settings.experimentalVersion",{version:"2.1.300"}));
    // 데몬이 이 PC에서 잰 것이 없으면 근거 줄도 없다.
    expect(chip.querySelector("[data-testid=mission-binding-trust-reason]")).toBeNull();

    // A probe observed an updated CLI; the registry no longer opens those capabilities.
    const unknown={supported:false,reason_code:"no_compatibility_evidence"};
    const save=vi.spyOn(client,"bindingSave");
    vi.spyOn(client,"bindingProbe").mockResolvedValue({binding:{...saved,revision:"2",runtime_version:"2.1.310",
      capabilities:{...saved.capabilities,structured_result:unknown,events:unknown,cancel:unknown,read_only:unknown,scoped_write:unknown}},models:[],installation:"verified"});
    click(buttonIn(handle.container,"missions.settings.probeNow"));await flushAsync();
    // 동의는 연결당 한 번(11 §3.4): 재동의를 요구하는 UI는 없고, 칩만 미확인으로 바뀐다.
    expect(handle.container.querySelector("[data-testid=mission-binding-trust]")?.getAttribute("data-trust")).toBe("unverified");
    expect(handle.container.textContent).toContain(t("missions.trust.unverified"));
    expect(handle.container.querySelector("[data-testid=mission-binding-reconsent]")).toBeNull();
    expect(handle.container.querySelector("[data-testid=mission-binding-consent]")).toBeNull();
    expect([...handle.container.querySelectorAll("button")].some(button=>button.textContent===t("missions.settings.experimentalAgree"))).toBe(false);
    expect(save).not.toHaveBeenCalled();
  } finally { handle.unmount(); }
});

it.each(["ko", "en"] as const)("%s 연결 하나에 신뢰 칩과 근거 한 줄을 붙이고, 목록에 없는 모델은 막지 않고 알린다", async language => {
  useI18nStore.setState({ language });
  const client=stubDetect(installMockClient());
  const base=newBinding("codex");
  const local={supported:true,reason_code:"local_probe"};
  // 데몬이 이 PC에서 잰 증거(11 §3.3)를 가진 연결 — 저장 목록으로 그대로 들어온다.
  const connection={...base,id:"binding-local",revision:"3",label:"Codex local",program:"/fixture/codex",model_id:"gpt-6-astra",runtime_version:"0.161.0",
    capabilities:{...base.capabilities,structured_result:local,events:local,cancel:local,read_only:local,scoped_write:local},
    local_evidence:{os:"macos",version:"0.161.0",model_id:"gpt-6-astra",probed_at:"2026-09-19T00:00:00Z",
      probe:{protocol_ok:true,sandbox_cases_passed:12,sandbox_cases_total:12,model_listed:false,failures:[]},
      runs:{succeeded_read_only:2,succeeded_write:1,cancelled:0,invalid_result:0,last_at:"2026-09-19T01:00:00Z"}}};
  client.bindingList=vi.fn(async()=>({bindings:[connection]}));
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    setValue(fieldIn(handle.container,"missions.settings.savedModels"),connection.id);
    await flushAsync();
    const chip=handle.container.querySelector("[data-testid=mission-binding-trust]")!;
    expect(chip.getAttribute("data-trust")).toBe("local");
    expect(chip.textContent).toContain(t("missions.trust.local"));
    expect(chip.querySelector("[data-testid=mission-binding-trust-reason]")?.textContent).toBe([
      t("missions.trust.protocolOk"),
      t("missions.trust.sandbox",{passed:12,total:12}),
      t("missions.trust.runs",{count:3}),
      t("missions.trust.probedAt",{at:new Date("2026-09-19T00:00:00Z").toLocaleString(language)}),
    ].join(" · "));
    // 모델이 CLI 목록에 없다는 경고는 알리기만 한다 — 동의도, 비활성도 없다.
    expect(chip.querySelector("[data-testid=mission-binding-model-unlisted]")?.textContent).toBe(t("missions.trust.modelNotListed"));
    expect(handle.container.querySelector("[data-testid=mission-binding-consent]")).toBeNull();
    expect(buttonIn(handle.container,"missions.settings.probeNow").disabled).toBe(false);
  } finally { handle.unmount(); }
});

it.each(["ko", "en"] as const)("%s 어느 증거도 열지 못한 저장 연결은 한 번의 실험적 동의를 받아 저장한다", async language => {
  useI18nStore.setState({ language });
  const client=stubDetect(installMockClient());
  const unknown={supported:false,reason_code:"no_compatibility_evidence"};
  const base=newBinding("codex");
  const saved=(await client.bindingSave({request_id:crypto.randomUUID(),expected_revision:"0",binding:{...base,label:"Codex astra",program:"/fixture/codex",model_id:"gpt-6-astra",
    runtime_version:"0.154.0",capabilities:{...base.capabilities,structured_result:unknown,events:unknown,cancel:unknown,read_only:unknown,scoped_write:unknown}}})).binding;
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    setValue(fieldIn(handle.container,"missions.settings.savedModels"),saved.id);
    await flushAsync();
    // No consent was ever given, so this is not a renewal: the first consent is asked for, naming the model.
    expect(handle.container.querySelector("[data-testid=mission-binding-reconsent]")).toBeNull();
    const consent=handle.container.querySelector("[data-testid=mission-binding-consent]")!;
    expect(consent.textContent).toContain(t("missions.settings.experimentalConsent",{model:"gpt-6-astra",version:"0.154.0"}));
    expect(consent.textContent).toContain(t("missions.quickSetup.experimental.risk",{version:"0.154.0"}));
    expect(handle.container.querySelector("[data-testid=mission-binding-trust]")?.getAttribute("data-trust")).toBe("unverified");
    const save=vi.spyOn(client,"bindingSave").mockImplementation(async params=>({binding:{...params.binding,revision:String(Number(params.expected_revision)+1)}}));
    click(consent.querySelector("[data-testid=mission-binding-consent-agree]")!);await flushAsync();
    expect(save).toHaveBeenCalledTimes(1);
    expect(save.mock.calls[0][0].binding).toMatchObject({id:saved.id,experimental_version:"0.154.0",runtime_version:"0.154.0"});
    expect(handle.container.querySelector("[data-testid=mission-binding-consent]")).toBeNull();
    expect(handle.container.querySelector("[data-testid=mission-settings-notice]")?.textContent).toBe(t("missions.settings.saved"));
  } finally { handle.unmount(); }
});

const teamRoles=["lead","builder","reviewer","integrator"] as const;
/** The role-picker's option groups as [group label, option texts]. */
function roleGroups(select: HTMLSelectElement): Array<[string, string[]]> {
  return [...select.querySelectorAll("optgroup")].map(group=>[group.label,[...group.querySelectorAll("option")].map(option=>option.textContent ?? "")]);
}

it("1M 컨텍스트 접미사가 붙은 alias는 Z.ai가 아니라 Claude Code 그룹에 남는다", async () => {
  // `~/.claude/settings.json`의 `"model": "opus[1m]"`이 configured_model_id로 들어온다.
  // 접미사를 떼기 전에는 `claude-`도 alias 완전일치도 아니어서, 모양 기반 분류가 이 id를
  // Z.ai 경로 후보로 보내고 Anthropic 그룹에서는 빼버렸다(같은 판정을 양쪽이 쓴다).
  const client=stubDetect(installMockClient(),[
    {...detectedRuntime("claude",[{id:"opus",efforts:[]}]),version:"2.1.271",configured_model_id:"opus[1m]",experimental_roles:[...teamRoles],
      alt_models:[{provider_id:"zai-coding-plan",models:[{id:"glm-5.3",efforts:[]}]}]},
  ]);
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    const card=handle.container.querySelector("[data-setting-id=missionTeams]")!;
    const lead=card.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.role.lead")}"]`)!;
    expect(roleGroups(lead)).toEqual([
      ["Claude Code",["opus[1m]","opus"]],
      ["Claude Code · Z.ai Coding Plan",["glm-5.3[1m]","glm-5.3-flash[1m]","glm-5.3"]],
    ]);
  } finally { handle.unmount(); }
});

it.each(["ko", "en"] as const)("%s 역할 선택에 감지된 모델이 런타임별로 나오고, 고른 모델의 연결을 팀 저장 때 만들어 동의와 함께 저장한다", async language => {
  useI18nStore.setState({ language });
  // Versions match the mock daemon's installed CLIs, so a consented connection comes back experimentally usable.
  const client=stubDetect(installMockClient(),[
    {...detectedRuntime("codex",[{id:"gpt-5.6-luna",efforts:[]},{id:"gpt-6-astra",efforts:[]}]),version:"0.154.0",proven_model_id:"gpt-5.6-luna",verified_roles:[...teamRoles],grade:"verified",experimental_roles:[...teamRoles]},
    {...detectedRuntime("claude",[{id:"opus",efforts:[]}]),version:"2.1.271",configured_model_id:"claude-opus-5",experimental_roles:[...teamRoles],
      alt_models:[{provider_id:"zai-coding-plan",models:[{id:"glm-5.3",efforts:[]}]}]},
    detectedRuntime("opencode",[]),
  ]);
  const save=vi.spyOn(client,"bindingSave");
  const probe=vi.spyOn(client,"bindingProbe");
  const templateSave=vi.spyOn(client,"templateSave");
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    const card=handle.container.querySelector("[data-setting-id=missionTeams]")!;
    const lead=card.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.role.lead")}"]`)!;
    // No saved connection yet: every detected model is on offer, grouped by runtime, in catalog order.
    // OpenCode is installed but has no stored credentials to reuse, so it says what is missing instead.
    expect(roleGroups(lead)).toEqual([
      ["Codex",["gpt-5.6-luna","gpt-6-astra"]],
      ["Claude Code",["claude-opus-5","opus"]],
      // The Z.ai route the CLI's own transcripts show — the ccg equivalent, launched from the
      // daemon's key store. Its models are pickable right away; the note names the key it needs.
      ["Claude Code · Z.ai Coding Plan",["glm-5.3[1m]","glm-5.3-flash[1m]","glm-5.3"]],
      ["OpenCode",[t("missions.settings.opencodeNeedsConnection")]],
    ]);
    expect(card.querySelector('[data-testid="team-hint-opencode"]')?.textContent).toBe(t("missions.settings.opencodeTeamHint"));
    expect(card.querySelector('[data-testid="team-hint-claude:zai-coding-plan"]')?.textContent).toBe(t("missions.settings.claudeZaiTeamHint"));
    expect(card.querySelector("[data-testid=team-experimental]")).toBeNull();
    const all=card.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.settings.allRoles")}"]`)!;
    const opus=[...lead.options].find(option=>option.textContent==="opus")!.value;
    setValue(all,opus);
    for (const role of teamRoles) expect(card.querySelector<HTMLSelectElement>(`select[aria-label="${t(`missions.role.${role}`)}"]`)!.value).toBe(opus);
    // Claude Code is unverified on this version: the connection would run experimentally, so the save waits for consent.
    const consent=card.querySelector<HTMLInputElement>("[data-testid=team-experimental-consent]")!;
    expect(consent).not.toBeNull();
    expect(card.textContent).toContain(t("missions.settings.teamExperimentalRisk",{models:"Claude Code · opus"}));
    expect(buttonIn(card,"missions.settings.saveTeam").disabled).toBe(true);
    expect(card.textContent).toContain(t("missions.settings.teamConsentRequired"));
    click(consent);
    expect(buttonIn(card,"missions.settings.saveTeam").disabled).toBe(false);
    setValue(card.querySelector("input")!,"Claude team");
    click(buttonIn(card,"missions.settings.saveTeam"));await flushAsync();
    // One connection for the one distinct pick, saved with the consent and checked before the team names it.
    expect(save).toHaveBeenCalledTimes(1);
    expect(save.mock.calls[0][0].binding).toMatchObject({runtime:"claude",label:"Claude Code · opus",program:"/fixture/claude",provider_id:"anthropic",model_id:"opus",
      auth_route:"subscription",credential_ref:null,endpoint_ref:null,enabled:true,experimental_version:"2.1.271"});
    const id=save.mock.calls[0][0].binding.id;
    expect(probe).toHaveBeenCalledWith({binding_id:id});
    expect(templateSave).toHaveBeenCalledTimes(1);
    const template=templateSave.mock.calls[0][0].template;
    expect(template.label).toBe("Claude team");
    expect(template.role_bindings).toEqual(teamRoles.map(role=>({role,primary_binding_id:id,fallback_binding_ids:[]})));
    expect(template.policy.allowed_binding_ids).toEqual([id]);
    expect(handle.container.querySelector("[data-testid=mission-settings-notice]")?.textContent).toBe(t("missions.settings.saved"));
    // The picks now name the saved connection.
    expect(lead.value).toBe(id);
    expect(roleGroups(lead)).toEqual([
      [t("missions.settings.savedConnections"),["Claude Code · opus · opus"]],
      ["Codex",["gpt-5.6-luna","gpt-6-astra"]],
      ["Claude Code",["claude-opus-5","opus"]],
      ["Claude Code · Z.ai Coding Plan",["glm-5.3[1m]","glm-5.3-flash[1m]","glm-5.3"]],
      ["OpenCode",[t("missions.settings.opencodeNeedsConnection")]],
    ]);
    expect(card.querySelector("[data-testid=team-experimental]")).toBeNull();
  } finally { handle.unmount(); }
});

it("새 연결의 추론 강도를 저장하고, 같은 모델을 다시 저장하면 연결을 재사용한다", async () => {
  const client=stubDetect(installMockClient(),[
    {...detectedRuntime("codex",[{id:"gpt-6-astra",efforts:["low","high"]}]),version:"0.154.0"},
    detectedRuntime("claude",[]),
    detectedRuntime("opencode",[]),
  ]);
  const save=vi.spyOn(client,"bindingSave");
  const probeSpy=vi.spyOn(client,"bindingProbe");
  const templateSave=vi.spyOn(client,"templateSave");
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    const card=handle.container.querySelector("[data-setting-id=missionTeams]")!;
    const all=card.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.settings.allRoles")}"]`)!;
    const astra=[...all.options].find(option=>option.textContent==="gpt-6-astra")!.value;
    setValue(all,astra);
    // The pick advertises effort rungs: one select for the one distinct pick, runtime default first.
    const effortSelect=card.querySelector<HTMLSelectElement>('select[aria-label^="Codex · gpt-6-astra"]')!;
    expect([...effortSelect.options].map(option=>option.value)).toEqual(["","low","high"]);
    setValue(effortSelect,"high");
    // stubDetect 픽스처의 검증이 unverified라 새 연결은 실험 취급 — 저장 전 동의가 필요하다.
    click(card.querySelector("[data-testid=team-experimental-consent]")!);
    setValue(card.querySelector("input")!,"Effort team");
    click(buttonIn(card,"missions.settings.saveTeam"));await flushAsync();
    expect(save).toHaveBeenCalledTimes(1);
    expect(save.mock.calls[0][0].binding).toMatchObject({runtime:"codex",model_id:"gpt-6-astra",effort:"high"});
    const savedId=save.mock.calls[0][0].binding.id;
    expect(templateSave).toHaveBeenCalledTimes(1);
    // Saving the same pick again reuses the connection: only a fresh probe, no second binding.
    setValue(card.querySelector("input")!,"Effort team");
    click(buttonIn(card,"missions.settings.saveTeam"));await flushAsync();
    expect(save).toHaveBeenCalledTimes(1);
    expect(probeSpy).toHaveBeenLastCalledWith({binding_id:savedId});
    expect(templateSave).toHaveBeenCalledTimes(2);
  } finally { handle.unmount(); }
});

it("offers an OpenCode connection's other models and clones its credentials for the pick", async () => {
  const client=stubDetect(installMockClient(),[{...detectedRuntime("opencode",[]),version:"1.18.30",suggested_provider_id:"zai-coding-plan"}]);
  const refs={credential_ref:"keyring:11111111-1111-4111-8111-111111111111",endpoint_ref:"11111111-1111-4111-8111-111111111111"};
  const source=(await client.bindingSave({request_id:crypto.randomUUID(),expected_revision:"0",binding:{...newBinding("opencode"),label:"Z.ai Coding",program:"/fixture/opencode",
    provider_id:"zai-coding-plan",model_id:"glm-5.3",auth_route:"subscription",...refs,enabled:true}})).binding;
  // The catalog of a stored OpenCode connection comes from a probe; here it names one model beyond the saved one.
  const originalProbe=client.bindingProbe.bind(client);
  const probe=vi.spyOn(client,"bindingProbe").mockImplementation(async params=>({...await originalProbe(params),models:[{id:"glm-5.3",efforts:[]},{id:"glm-5-turbo",efforts:[]}]}));
  const save=vi.spyOn(client,"bindingSave");
  const templateSave=vi.spyOn(client,"templateSave");
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync(2);
    // The catalog was asked for once, for the one stored connection.
    expect(probe).toHaveBeenCalledTimes(1);
    expect(probe).toHaveBeenCalledWith({binding_id:source.id});
    const card=handle.container.querySelector("[data-setting-id=missionTeams]")!;
    const lead=card.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.role.lead")}"]`)!;
    expect(roleGroups(lead)).toEqual([
      [t("missions.settings.savedConnections"),["Z.ai Coding · glm-5.3"]],
      // OpenCode 그룹은 probe된 전체 목록이라 현재 모델(glm-5.3)도 남아 있다.
      ["OpenCode",["glm-5.3","glm-5-turbo"]],
    ]);
    expect(card.querySelector("[data-testid=team-hint-opencode]")).toBeNull();
    const turbo=[...lead.options].find(option=>option.textContent==="glm-5-turbo")!.value;
    setValue(card.querySelector<HTMLSelectElement>(`select[aria-label="${t("missions.settings.allRoles")}"]`)!,turbo);
    // OpenCode has no evidence at all, so the new connection is experimental and needs consent.
    click(card.querySelector("[data-testid=team-experimental-consent]")!);
    setValue(card.querySelector("input")!,"Turbo team");
    click(buttonIn(card,"missions.settings.saveTeam"));await flushAsync();
    expect(save).toHaveBeenCalledTimes(1);
    expect(save.mock.calls[0][0].binding).toMatchObject({runtime:"opencode",label:"OpenCode · glm-5-turbo",program:"/fixture/opencode",provider_id:"zai-coding-plan",
      model_id:"glm-5-turbo",auth_route:"subscription",...refs,enabled:true,experimental_version:"1.18.30"});
    const id=save.mock.calls[0][0].binding.id;
    expect(templateSave.mock.calls[0][0].template.role_bindings.map(entry=>entry.primary_binding_id)).toEqual([id,id,id,id]);
    expect(lead.value).toBe(id);
  } finally { handle.unmount(); }
});

it("offers the detected runtime's models before any probe runs", async () => {
  const client=stubDetect(installMockClient(),[detectedRuntime("codex",[{id:"gpt-6-astra",efforts:[]},{id:"gpt-5.6-sol",efforts:[]}])]);
  const saved=(await client.bindingSave({request_id:crypto.randomUUID(),expected_revision:"0",
    binding:{...newBinding(),label:"Existing",program:"/fixture/cli",model_id:"model",enabled:true}})).binding;
  const probe=vi.spyOn(client,"bindingProbe");
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    expect(modelChoicesIn(handle.container)).toEqual(["gpt-6-astra","gpt-5.6-sol"]);
    // Editing a stored connection shows the same candidates — the user never has to check the connection
    // first — and keeps its own id selectable even though nothing advertises it.
    setValue(fieldIn(handle.container,"missions.settings.savedModels"),saved.id);
    await flushAsync();
    expect(modelChoicesIn(handle.container)).toEqual(["model","gpt-6-astra","gpt-5.6-sol"]);
    expect((fieldIn(handle.container,"missions.settings.model") as HTMLSelectElement).value).toBe("model");
    expect(probe).not.toHaveBeenCalled();
    // Another runtime advertises its own list (here: nothing detected for Claude Code): back to plain text.
    setValue(fieldIn(handle.container,"missions.settings.runtime"),"claude");
    expect(modelChoicesIn(handle.container)).toEqual([]);
    expect(manualModelIn(handle.container)).not.toBeNull();
  } finally { handle.unmount(); }
});

it("merges probe and detect candidates, listing a shared model id once", async () => {
  const client=stubDetect(installMockClient(),[detectedRuntime("codex",[{id:"gpt-6-astra",efforts:[]},{id:"detect-only",efforts:[]}])]);
  const saved=(await client.bindingSave({request_id:crypto.randomUUID(),expected_revision:"0",
    binding:{...newBinding(),label:"Existing",program:"/fixture/cli",model_id:"model",enabled:true}})).binding;
  vi.spyOn(client,"bindingProbe").mockResolvedValue({binding:{...saved,revision:"2"},
    models:[{id:"probe-only",efforts:[]},{id:"gpt-6-astra",efforts:[]}],installation:"verified"});
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    setValue(fieldIn(handle.container,"missions.settings.savedModels"),saved.id);
    await flushAsync();
    expect(modelChoicesIn(handle.container)).toEqual(["model","gpt-6-astra","detect-only"]);
    click(buttonIn(handle.container,"missions.settings.probeNow"));await flushAsync();
    // After the stored id, what this connection answered comes first; the detected list fills in the rest without repeating an id.
    expect(modelChoicesIn(handle.container)).toEqual(["model","probe-only","gpt-6-astra","detect-only"]);
  } finally { handle.unmount(); }
});

it("keeps the connection form working when runtime detection fails", async () => {
  const client=installMockClient();
  client.runtimeDetect=vi.fn(async (): Promise<RuntimeDetectResult> => { throw new Error("detect unavailable"); });
  const save=vi.spyOn(client,"bindingSave");
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    // Candidates are a convenience: losing them must not turn the settings screen into an error.
    expect(handle.container.querySelector("[data-testid=mission-settings-error]")).toBeNull();
    expect(modelChoicesIn(handle.container)).toEqual([]);
    const modelInput=fieldIn(handle.container,"missions.settings.model") as HTMLInputElement;
    setValue(fieldIn(handle.container,"missions.settings.label"),"Manual codex");
    setValue(fieldIn(handle.container,"missions.settings.program"),"/fixture/codex");
    setValue(modelInput,"typed-model");
    click(buttonIn(handle.container,"missions.settings.saveModel"));await flushAsync();
    expect(save.mock.calls[0][0].binding).toMatchObject({label:"Manual codex",model_id:"typed-model"});
    expect(handle.container.querySelector("[data-testid=mission-settings-error]")).toBeNull();
    expect(handle.container.querySelector("[data-testid=mission-settings-notice]")?.textContent).toBe(t("missions.settings.saved"));
  } finally { handle.unmount(); }
});

it.each(["ko", "en"] as const)("%s 모델이 광고한 추론 강도를 고르게 하고 연결에 저장한다", async language => {
  useI18nStore.setState({ language });
  const client=stubDetect(installMockClient(),[detectedRuntime("codex",[{id:"gpt-6-astra",efforts:["low","high"]},{id:"gpt-5.6-sol",efforts:[]}])]);
  const save=vi.spyOn(client,"bindingSave");
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    setValue(fieldIn(handle.container,"missions.settings.label"),"Codex astra");
    setValue(fieldIn(handle.container,"missions.settings.program"),"/fixture/codex");
    // No model chosen yet, so there is no advertised effort to choose from either.
    expect(effortSelectIn(handle.container)).toBeNull();
    setValue(fieldIn(handle.container,"missions.settings.model"),"gpt-6-astra");
    const effort=effortSelectIn(handle.container)!;
    expect([...effort.options].map(option=>option.value)).toEqual(["","low","high"]);
    expect(effort.options[0].textContent).toBe(t("missions.settings.effortDefault"));
    expect(effort.value).toBe("");
    setValue(effort,"high");
    click(buttonIn(handle.container,"missions.settings.saveModel"));await flushAsync();
    expect(save.mock.calls[0][0].binding).toMatchObject({model_id:"gpt-6-astra",effort:"high"});
    // The empty option means the runtime default, which is stored as no effort at all.
    setValue(effortSelectIn(handle.container)!,"");
    click(buttonIn(handle.container,"missions.settings.saveModel"));await flushAsync();
    expect(save.mock.calls[1][0].binding.effort).toBeNull();
  } finally { handle.unmount(); }
});

it("hides the effort select when neither the model nor the connection has an effort", async () => {
  const client=stubDetect(installMockClient(),[detectedRuntime("codex",[{id:"gpt-6-astra",efforts:[]}])]);
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    const modelSelect=fieldIn(handle.container,"missions.settings.model") as HTMLSelectElement;
    setValue(modelSelect,"gpt-6-astra");
    // The model is a known candidate; it just advertises no effort, so an empty dropdown is not shown.
    expect(modelChoicesIn(handle.container)).toEqual(["gpt-6-astra"]);
    expect(effortSelectIn(handle.container)).toBeNull();
    setValue(modelSelect,CUSTOM_MODEL_OPTION);
    setValue(manualModelIn(handle.container)!,"a-typed-model-not-in-the-list");
    expect(effortSelectIn(handle.container)).toBeNull();
  } finally { handle.unmount(); }
});

it("keeps a stored effort the runtime no longer advertises and saves it unchanged", async () => {
  const client=stubDetect(installMockClient(),[detectedRuntime("codex",[{id:"gpt-6-astra",efforts:["low"]}])]);
  const saved=(await client.bindingSave({request_id:crypto.randomUUID(),expected_revision:"0",
    binding:{...newBinding(),label:"Existing",program:"/fixture/codex",model_id:"gpt-6-astra",effort:"xhigh",enabled:true}})).binding;
  const save=vi.spyOn(client,"bindingSave");
  const handle=renderUi(<MissionSettings client={client}/>);
  try {
    await flushAsync();
    setValue(fieldIn(handle.container,"missions.settings.savedModels"),saved.id);
    await flushAsync();
    const effort=effortSelectIn(handle.container)!;
    expect([...effort.options].map(option=>option.value)).toEqual(["","low","xhigh"]);
    expect(effort.value).toBe("xhigh");
    click(buttonIn(handle.container,"missions.settings.saveModel"));await flushAsync();
    expect(save.mock.calls[0][0].binding).toMatchObject({id:saved.id,effort:"xhigh"});
  } finally { handle.unmount(); }
});
