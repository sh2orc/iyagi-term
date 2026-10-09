import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`불명 실행의 영향을 확인한 뒤 ${layout} 화면에서 확정한다`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const missionId = await seedAndOpen(page, { agents: 1 });
    const previous = await page.evaluate(async id => {
      const api = window.__mockDaemon!;
      await api.stageForAcceptance(id, { allPassed: true });
      const client = api.client as MockDaemonClient;
      const snapshot = await client.missionSnapshot({ mission_id: id, snapshot_id: null, cursor: null });
      const run = snapshot.entities.flatMap(e => "Run" in e ? [e.Run] : [])[0];
      const unknown = { ...run, state: "unknown" as const, reconciliation_ref: run.context_ref };
      client.seedMissionEntities(id, [{ Run: unknown }]);
      return unknown;
    }, missionId);
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    await page.getByRole("button", { name: "결과", exact: true }).click();
    const review = page.getByTestId("reconciled-accept-review");
    await expect(review).toContainText("과거 실행의 제공자 결과와 외부 영향은 알 수 없습니다");
    const accept = page.getByTestId("accept-button");
    await expect(accept).toBeDisabled();
    const checkbox = review.getByRole("checkbox");
    await expect(checkbox).not.toBeChecked();
    await checkbox.check();
    // 사용자 확인(human check)과 관찰 검증 확인도 모두 체크해야 한다.
    await expect(accept).toBeDisabled();
    for (const box of await page.getByTestId("human-confirmations").getByRole("checkbox").all()) {
      if (!(await box.isChecked())) await box.check();
    }
    await expect(accept).toBeEnabled();
    await accept.click();
    const ok = page.getByTestId("accept-confirm-ok");
    await expect(ok).toBeEnabled();
    await ok.scrollIntoViewIfNeeded();
    await expect(ok).toBeInViewport({ ratio: 0.99 });
    await checkbox.scrollIntoViewIfNeeded();
    await expect(checkbox).toBeInViewport({ ratio: 0.99 });
    const width = await page.evaluate(() => ({ client: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(width.scroll).toBeLessThanOrEqual(width.client + 1);
    await page.screenshot({ path: testInfo.outputPath(`reconciled-acceptance-${layout}.png`) });
    await ok.click();
    await expect(page.getByTestId("accept-button")).toHaveCount(0);
    const current = await page.evaluate(async id => (await window.__mockDaemon!.client.missionSnapshot({ mission_id: id, snapshot_id: null, cursor: null })).entities, missionId);
    expect(current.flatMap(e => "Run" in e && e.Run.id === previous.id ? [e.Run] : [])[0]).toEqual(previous);
    expect(current.flatMap(e => "Mission" in e ? [e.Mission] : [])[0]).toMatchObject({ state: "completed" });
  });
}
