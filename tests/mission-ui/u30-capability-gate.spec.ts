import { expect, test } from "@playwright/test";
import type { Binding } from "../../src/generated/Binding";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`호환성 대기에서 ${layout} 화면의 검증된 모델만 선택한다`, async ({page}, testInfo) => {
    await page.setViewportSize({width:layout === "700" ? 700 : 1440, height:1000});
    await prepare(page);
    const id = await seedAndOpen(page, {agents:1});
    const prior = await page.evaluate(async missionId => {
      const client = window.__mockDaemon!.client as MockDaemonClient;
      const snapshot = await client.missionSnapshot({mission_id:missionId,snapshot_id:null,cursor:null});
      const mission = snapshot.entities.flatMap(e=>"Mission" in e ? [e.Mission] : [])[0];
      const task = snapshot.entities.flatMap(e=>"Task" in e ? [e.Task] : [])[0];
      const run = snapshot.entities.flatMap(e=>"Run" in e ? [e.Run] : [])[0];
      const capabilities = Object.fromEntries(["structured_result","events","cancel","resume","steer","approval_reply","read_only","scoped_write","model_listing","usage","native_terminal_attach"].map(key=>[key,{supported:true,reason_code:null}])) as Binding["capabilities"];
      const supported: Binding = {id:crypto.randomUUID(), revision:"0",label:"검증된 모델",runtime:"codex",program:"/fixture/codex",runtime_version:"fixture-v1",provider_id:"openai",model_id:"fixture",effort:null,auth_route:"subscription",credential_ref:null,endpoint_ref:null,capabilities,checked_at:"2026-09-16T00:00:00Z",enabled:true,estimated_run_cost_usd_micros:null,resource_policy:{reservation_bytes:"1",cpu_slots:1,enforcement:"observe",memory_max_bytes:null,cpu_max_cores:null,pids_max:null},experimental_version:null};
      const unsupported = {...supported,id:crypto.randomUUID(),label:"미검증 모델",capabilities:{...capabilities,scoped_write:{supported:false,reason_code:"unverified"}}};
      for (const binding of [supported,unsupported]) await client.bindingSave({request_id:crypto.randomUUID(),expected_revision:"0",binding});
      const ended = {...run,state:"cancelled" as const,ended_at:"2026-09-16T00:00:00Z"};
      client.seedMissionEntities(missionId,[
        {Mission:{...mission,phase:"implementing",policy:{...mission.policy,allowed_binding_ids:[supported.id,unsupported.id]}}},
        {Task:{...task,kind:"implement",role:"builder",state:"blocked",active_run_id:null,binding_id:unsupported.id,blocked_code:"capability_scoped_write"}},
        {Run:ended},
      ]);
      return {taskId:task.id,run:ended,supported:supported.id};
    },id);
    if (layout === "zoom200") await page.evaluate(()=>{document.body.style.zoom="2";});
    await page.getByRole("button",{name:"할 일",exact:true}).click();
    await page.getByTestId("team-row").first().click();
    await expect(page.getByTestId("capability-notice")).toContainText("시도 횟수와 예산을 쓰지 않습니다");
    await page.getByTestId("task-reassign").click();
    const picker = page.getByTestId("reassign-picker");
    await expect(picker.getByRole("button",{name:/미검증 모델/})).toBeDisabled();
    const supported = picker.getByRole("button",{name:/검증된 모델/});
    await expect(supported).toBeEnabled();
    await supported.scrollIntoViewIfNeeded();
    expect(await page.evaluate(()=>document.documentElement.scrollWidth-document.documentElement.clientWidth)).toBeLessThanOrEqual(1);
    await page.screenshot({path:testInfo.outputPath(`capability-${layout}.png`)});
    await supported.click();
    const after = await page.evaluate(async missionId => (await (window.__mockDaemon!.client as MockDaemonClient).missionSnapshot({mission_id:missionId,snapshot_id:null,cursor:null})).entities,id);
    expect(after.flatMap(e=>"Task" in e && e.Task.id===prior.taskId ? [e.Task] : [])[0].binding_id).toBe(prior.supported);
    expect(after.flatMap(e=>"Run" in e ? [e.Run] : [])).toEqual([prior.run]);
  });
}
