import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`비용 대기 정책을 ${layout} 화면에서 변경한다`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const missionId = await seedAndOpen(page, { agents: 1 });
    await page.evaluate(async id=>{
      const api=window.__mockDaemon!,client=api.client as MockDaemonClient;
      const snapshot=await client.missionSnapshot({mission_id:id,snapshot_id:null,cursor:null});
      const task=snapshot.entities.flatMap(e=>"Task" in e?[e.Task]:[])[0];
      const run=snapshot.entities.flatMap(e=>"Run" in e?[e.Run]:[])[0];
      const decisionId=await api.openDecision(id,{question:"Cost admission fixture",options:[{id:"stop_cost_mission",label:"Stop mission"}]});
      const next=await client.missionSnapshot({mission_id:id,snapshot_id:null,cursor:null});
      const decision=next.entities.flatMap(e=>"Decision" in e&&e.Decision.id===decisionId?[e.Decision]:[])[0];
      client.seedMissionEntities(id,[
        {Task:{...task,state:"blocked",active_run_id:null,blocked_code:"cost_limit"}},
        {Run:{...run,state:"succeeded",ended_at:new Date().toISOString()}},
        {Decision:{...decision,kind:"budget",blocking:false,affected_task_ids:[task.id],requesting_run_id:null}},
      ]);
    },missionId);
    if(layout==="zoom200")await page.evaluate(()=>{document.body.style.zoom="2";});
    await expect(page.getByTestId("decision-banner-text")).toContainText("비용 정책");
    await page.getByTestId("decision-banner-open").click();
    const panel=page.getByTestId("decision-panel");
    await expect(panel).toContainText("관련 없는 할 일은 계속 진행");
    // 비용 결정은 "AI 작업 한도 조정"으로 한도 편집기를 연다.
    await panel.getByTestId("decision-adjust-limits").click();
    const editor=page.getByTestId("policy-limits-editor");
    await editor.getByTestId("limits-cost").fill("4.000001");
    await editor.getByTestId("limits-unknown-cost").selectOption("allow_with_notice");
    const save=editor.getByRole("button",{name:"한도 저장",exact:true});
    await expect(save).toBeVisible();
    await save.click();
    const status = editor.getByRole("status");
    await expect(status).toContainText("저장했습니다");
    await status.scrollIntoViewIfNeeded();
    // Allow subpixel scroll rounding at 200% zoom while requiring the whole line.
    await expect(status).toBeInViewport({ ratio: 0.99 });
    await page.getByTestId("toggle-lead").click();
    await expect(page.getByTestId("lead-composer")).toBeInViewport({ ratio: 0.99 });
    const value=await page.evaluate(async id=>{
      const snapshot=await window.__mockDaemon!.client.missionSnapshot({mission_id:id,snapshot_id:null,cursor:null});
      return snapshot.entities.flatMap(e=>"Mission" in e?[e.Mission.policy]:[])[0];
    },missionId);
    expect(value.max_cost_usd_micros).toBe("4000001");
    expect(value.unknown_cost).toBe("allow_with_notice");
    const size=await page.evaluate(()=>({width:document.documentElement.clientWidth,scroll:document.documentElement.scrollWidth}));
    expect(size.scroll).toBeLessThanOrEqual(size.width+1);
    await page.screenshot({path:testInfo.outputPath(`cost-${layout}.png`),fullPage:false});
  });
}
