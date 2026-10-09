import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`계획 수리 대기와 모델 변경을 ${layout} 화면에서 확인한다`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const id = await seedAndOpen(page, { agents: 1 });
    await page.evaluate(async missionId => {
      const client = window.__mockDaemon!.client as MockDaemonClient;
      const snapshot = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      const task = snapshot.entities.flatMap(e => "Task" in e ? [e.Task] : [])[0];
      const run = snapshot.entities.flatMap(e => "Run" in e ? [e.Run] : [])[0];
      client.seedMissionEntities(missionId, [
        { Task: { ...task, kind: "plan", role: "lead", state: "blocked", active_run_id: null, blocked_code: "plan_format_repair", dispatch_after_unix_ms: null } },
        { Run: { ...run, state: "failed", failure_code: "RESULT_INVALID", retry_evidence: { basis: "plan_format_rejected", plan_revision: 0, rejected_result_ref: null }, ended_at: new Date().toISOString() } },
      ]);
    }, id);
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    await page.getByTestId("toggle-detail").click();
    await expect(page.getByText("계획 형식 수정 대기", { exact: true })).toBeVisible();
    await page.getByTestId("team-row").first().click();
    const notice = page.getByTestId("plan-repair-notice");
    await expect(notice).toContainText("최대 두 번");
    await expect(notice).toContainText("일시정지 중에는 기다리며");
    await notice.scrollIntoViewIfNeeded();
    await expect(notice).toBeInViewport({ ratio: 0.99 });
    await expect(page.getByTestId("task-retry")).toHaveCount(0);
    const reassign = page.getByTestId("task-reassign");
    await expect(reassign).toBeEnabled();
    await reassign.scrollIntoViewIfNeeded();
    await expect(reassign).toBeInViewport({ ratio: 0.99 });
    const size = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(size.scroll).toBeLessThanOrEqual(size.width + 1);
    await page.screenshot({ path: testInfo.outputPath(`plan-repair-${layout}.png`) });
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("side-sheet")).toHaveCount(0);
    await expect(page.getByTestId("team-row").first()).toBeFocused();
  });
}
