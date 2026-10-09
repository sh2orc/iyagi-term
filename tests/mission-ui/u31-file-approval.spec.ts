import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`파일 승인에서 경로와 변경 내용을 ${layout} 화면으로 검토한다`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const id = await seedAndOpen(page, { agents: 1 });
    await page.evaluate(async missionId => {
      const api = window.__mockDaemon!;
      const client = api.client as MockDaemonClient;
      const decisionId = await api.openDecision(missionId, { blocking: true,
        question: JSON.stringify({ provider_request_id: "private-wire-id", question: JSON.stringify({
          type: "file_change", reason: "요청한 파일을 추가합니다.", grant_root: null, details_available: true,
          changes: [{ path: "/workspace/한국어/아주-긴-경로/" + "file-".repeat(18) + ".txt", kind: { type: "add" },
            diff: "+<script>window.untrusted = true</script>\n+" + "긴 변경 내용 ".repeat(45) }],
        }) }), options: [{ id: "accept", label: "승인" }, { id: "decline", label: "거절" }],
      });
      const snapshot = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      const decision = snapshot.entities.flatMap(e => "Decision" in e && e.Decision.id === decisionId ? [e.Decision] : [])[0];
      client.seedMissionEntities(missionId, [{ Decision: { ...decision, kind: "approval" } }]);
    }, id);
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    await expect(page.getByTestId("decision-banner-text")).toContainText("다음 파일 변경을 허용할까요?");
    await expect(page.getByTestId("decision-banner-text")).not.toContainText("private-wire-id");
    await page.getByTestId("decision-banner-open").click();
    const panel = page.getByTestId("decision-panel");
    const changes = panel.getByTestId("file-change-approval");
    await expect(changes).toContainText("요청한 파일을 추가합니다.");
    await expect(changes).toContainText("<script>window.untrusted = true</script>");
    await expect(changes.locator("script")).toHaveCount(0);
    await expect(panel).not.toContainText("private-wire-id");
    const accept = panel.getByRole("button", { name: "승인", exact: true });
    await accept.scrollIntoViewIfNeeded();
    await expect(accept).toBeEnabled();
    await expect(accept).toBeInViewport({ ratio: 0.99 });
    const width = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(width.scroll).toBeLessThanOrEqual(width.width + 1);
    await page.screenshot({ path: testInfo.outputPath(`file-approval-${layout}.png`) });
    await accept.click();
    await expect(accept).toBeDisabled();
  });
}
