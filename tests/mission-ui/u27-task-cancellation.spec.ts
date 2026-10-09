import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`필수 작업 취소 후 조건을 확인하고 ${layout} 화면에서 명시적으로 재시도한다`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const id = await seedAndOpen(page, { agents: 1 });
    const prior = await page.evaluate(async missionId => {
      const client = window.__mockDaemon!.client as MockDaemonClient;
      const snapshot = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      const mission = snapshot.entities.flatMap(e => "Mission" in e ? [e.Mission] : [])[0];
      const task = snapshot.entities.flatMap(e => "Task" in e ? [e.Task] : [])[0];
      const source = snapshot.entities.flatMap(e => "Run" in e ? [e.Run] : [])[0];
      const run = { ...source, state: "cancelled" as const, ended_at: "2026-09-16T00:00:00Z" };
      client.seedMissionEntities(missionId, [
        { Mission: { ...mission, phase: "implementing" } },
        { Task: { ...task, kind: "implement", role: "builder", required: true, active_run_id: null, state: "cancelled" } },
        { Run: run },
      ]);
      return { taskId: task.id, run, requirements: mission.requirements };
    }, id);
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    await page.getByTestId("toggle-detail").click();
    await page.getByTestId("team-row").first().click();
    const notice = page.getByTestId("required-cancelled-notice");
    await expect(notice).toContainText("필수 할 일을 취소해도 완료 조건은 그대로입니다");
    const retry = page.getByTestId("task-retry");
    await retry.scrollIntoViewIfNeeded();
    await expect(retry).toBeInViewport({ ratio: 0.99 });
    await expect(notice).toBeInViewport({ ratio: 0.99 });
    await expect(retry).toBeEnabled();
    const width = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(width.scroll).toBeLessThanOrEqual(width.width + 1);
    await page.screenshot({ path: testInfo.outputPath(`task-cancellation-${layout}.png`) });
    await retry.click();
    await expect(retry).toHaveCount(0);
    const after = await page.evaluate(async missionId => (await window.__mockDaemon!.client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null })).entities, id);
    expect(after.flatMap(e => "Task" in e && e.Task.id === prior.taskId ? [e.Task] : [])[0]).toMatchObject({ state: "ready", required: true });
    expect(after.flatMap(e => "Run" in e ? [e.Run] : [])).toEqual([prior.run]);
    expect(after.flatMap(e => "Mission" in e ? [e.Mission.requirements] : [])[0]).toEqual(prior.requirements);
  });
}
