import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`자동 통합의 안내와 명시적 재시도를 ${layout} 화면에서 확인한다`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const id = await seedAndOpen(page, { agents: 1 });
    const prior = await page.evaluate(async missionId => {
      const client = window.__mockDaemon!.client as MockDaemonClient;
      const snapshot = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      const mission = snapshot.entities.flatMap(e => "Mission" in e ? [e.Mission] : [])[0];
      const task = snapshot.entities.flatMap(e => "Task" in e ? [e.Task] : [])[0];
      const source = snapshot.entities.flatMap(e => "Run" in e ? [e.Run] : [])[0];
      const run = { ...source, state: "cancelled" as const, binding_snapshot: null,
        requested_model: null, observed_model: null, ended_at: "2026-09-16T00:00:00Z" };
      client.seedMissionEntities(missionId, [
        { Mission: { ...mission, phase: "integrating" } },
        { Task: { ...task, title: "작업 결과 통합", kind: "integrate", role: null, binding_id: null, active_run_id: null, state: "cancelled" } },
        { Run: run },
      ]);
      return { taskId: task.id, run };
    }, id);
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    await page.getByTestId("toggle-detail").click();
    await page.getByTestId("team-row").first().click();
    const notice = page.getByTestId("integration-instructions");
    await expect(notice).toContainText("자동 통합 단계입니다. 변경 지시는 Lead에게 보내세요.");
    await notice.scrollIntoViewIfNeeded();
    await expect(notice).toBeInViewport({ ratio: 0.99 });
    await expect(page.getByTestId("instruct-toggle")).toHaveCount(0);
    await expect(page.getByTestId("task-composer-input")).toHaveCount(0);
    await expect(page.getByTestId("task-reassign")).toHaveCount(0);
    const retry = page.getByTestId("task-retry");
    await retry.scrollIntoViewIfNeeded();
    await expect(retry).toBeInViewport({ ratio: 0.99 });
    const width = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(width.scroll).toBeLessThanOrEqual(width.width + 1);
    await page.screenshot({ path: testInfo.outputPath(`integration-${layout}.png`) });
    await retry.click();
    await expect(retry).toHaveCount(0);
    const after = await page.evaluate(async missionId => {
      const client = window.__mockDaemon!.client as MockDaemonClient;
      const snapshot = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      return { tasks: snapshot.entities.flatMap(e => "Task" in e ? [e.Task] : []), runs: snapshot.entities.flatMap(e => "Run" in e ? [e.Run] : []) };
    }, id);
    expect(after.tasks.find(t => t.id === prior.taskId)).toMatchObject({ state: "ready", binding_id: null });
    expect(after.runs).toEqual([prior.run]);
  });
}
