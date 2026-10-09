import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`자동 재시도 대기와 모델 변경을 ${layout} 화면에서 확인한다`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const id = await seedAndOpen(page, { agents: 1 });
    await page.evaluate(async missionId => {
      const client = window.__mockDaemon!.client as MockDaemonClient;
      const snapshot = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      const task = snapshot.entities.flatMap(e => "Task" in e ? [e.Task] : [])[0];
      const run = snapshot.entities.flatMap(e => "Run" in e ? [e.Run] : [])[0];
      client.seedMissionEntities(missionId, [
        { Task: { ...task, state: "blocked", active_run_id: null, blocked_code: "transient_retry", dispatch_after_unix_ms: String(Date.UTC(2026, 8, 16, 10, 30)) } },
        { Run: { ...run, state: "failed", dispatch_state: "may_have_sent", provider_session_id: null, provider_turn_id: null, failure_code: "PROVIDER_UNAVAILABLE", retry_evidence: { basis: "request_not_submitted", observed_at_unix_ms: String(Date.UTC(2026, 8, 16, 10, 30) - 3000), retry_after_unix_ms: String(Date.UTC(2026, 8, 16, 10, 30)) }, ended_at: new Date().toISOString() } },
      ]);
    }, id);
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    await page.getByTestId("toggle-detail").click();
    await expect(page.getByText("자동 재시도 대기 · 상세에서 시각 확인", { exact: true })).toBeVisible();
    await page.getByTestId("team-row").first().click();
    const notice = page.getByTestId("retry-notice");
    await expect(notice).toContainText("모델에 요청을 보내기 전에");
    await notice.scrollIntoViewIfNeeded();
    await expect(notice).toBeInViewport({ ratio: 0.99 });
    await expect(page.getByTestId("task-retry")).toHaveCount(0);
    const reassign = page.getByTestId("task-reassign");
    await expect(reassign).toBeEnabled();
    await reassign.scrollIntoViewIfNeeded();
    await expect(reassign).toBeInViewport({ ratio: 0.99 });
    const bounds = await page.evaluate(() => {
      const pageRect = document.querySelector('[data-testid="mission-page"]')!.getBoundingClientRect();
      const sheetRect = document.querySelector(".mission-workspace-content")!.getBoundingClientRect();
      return { pageTop: pageRect.top, pageBottom: pageRect.bottom, sheetTop: sheetRect.top, sheetBottom: sheetRect.bottom };
    });
    expect(bounds.sheetTop).toBeGreaterThanOrEqual(bounds.pageTop - 1);
    expect(bounds.sheetBottom).toBeLessThanOrEqual(bounds.pageBottom + 1);
    const size = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(size.scroll).toBeLessThanOrEqual(size.width + 1);
    await page.screenshot({ path: testInfo.outputPath(`retry-${layout}.png`) });
    await page.keyboard.press("Escape");
    await expect(page.locator(".mission-workspace-content")).toHaveCount(0);
    await expect(page.getByTestId("team-row").first()).toBeFocused();
  });
}
