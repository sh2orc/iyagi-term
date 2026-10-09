import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`실패 복구 결정은 ${layout} 화면에서 읽고 답할 수 있다`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const missionId = await seedAndOpen(page, { agents: 1 });
    await page.evaluate(async (id) => {
      const api = window.__mockDaemon!;
      const client = api.client as MockDaemonClient;
      const snapshot = await client.missionSnapshot({ mission_id: id, snapshot_id: null, cursor: null });
      const task = snapshot.entities.flatMap((e) => "Task" in e ? [e.Task] : [])[0];
      const run = snapshot.entities.flatMap((e) => "Run" in e ? [e.Run] : [])[0];
      const decisionId = await api.openDecision(id, { question: "Fixture task failure", options: [
        { id: "retry_failed_task", label: "Host retry" }, { id: "stop_failed_mission", label: "Host stop" },
      ] });
      const next = await client.missionSnapshot({ mission_id: id, snapshot_id: null, cursor: null });
      const decision = next.entities.flatMap((e) => "Decision" in e && e.Decision.id === decisionId ? [e.Decision] : [])[0];
      client.seedMissionEntities(id, [
        { Task: { ...task, state: "failed", active_run_id: null, blocked_code: "AuthRequired" } },
        { Run: { ...run, state: "failed", failure_code: "AUTH_REQUIRED", ended_at: new Date().toISOString() } },
        { Decision: { ...decision, kind: "recovery", affected_task_ids: [task.id], requesting_run_id: run.id } },
      ]);
    }, missionId);
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    await expect(page.getByTestId("decision-banner-text")).toContainText("복구 방법을 선택하세요");
    await expect(page.getByTestId("mission-elapsed")).toBeVisible();
    await expect(page.getByTestId("mission-elapsed")).toContainText("활성 시간");
    await page.getByTestId("decision-banner-open").click();
    const panel = page.getByTestId("decision-panel");
    await expect(panel).toContainText("AUTH_REQUIRED");
    await expect(panel).toContainText("관련 없는 할 일은 계속 진행합니다");
    const retry = panel.getByRole("button", { name: "원인 해결 후 다시 시도", exact: true });
    await expect(retry).toBeVisible();
    await expect(panel.getByRole("button", { name: "AI 작업 중단(실패로 기록)", exact: true })).toBeVisible();
    const size = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(size.scroll).toBeLessThanOrEqual(size.width + 1);
    await page.screenshot({ path: testInfo.outputPath(`recovery-${layout}.png`), fullPage: false });
    await retry.click();
    await expect(panel).toContainText("답변됨");
    await expect(retry).toBeDisabled();
  });
}
