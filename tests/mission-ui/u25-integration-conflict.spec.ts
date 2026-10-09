import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`통합 담당과 충돌 해결 요청을 ${layout} 화면에서 확인한다`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const id = await seedAndOpen(page, { agents: 1 });
    const prior = await page.evaluate(async missionId => {
      const api = window.__mockDaemon!;
      const client = api.client as MockDaemonClient;
      const snapshot = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      const mission = snapshot.entities.flatMap(e => "Mission" in e ? [e.Mission] : [])[0];
      const task = snapshot.entities.flatMap(e => "Task" in e ? [e.Task] : [])[0];
      const run = snapshot.entities.flatMap(e => "Run" in e ? [e.Run] : [])[0];
      const { binding } = await client.bindingSave({ request_id: crypto.randomUUID(), expected_revision: "0",
        binding: { ...run.binding_snapshot!, id: crypto.randomUUID(), revision: "0", label: "통합 담당 모델", model_id: "integrator-model" } });
      const decisionId = await api.openDecision(missionId, { question: JSON.stringify({ phase: "integration_conflict", conflict: { paths: ["src/shared.ts", "src/한국어/긴-경로-확인을-위한-충돌-파일-이름-입니다.ts"] } }), blocking: true, options: [
        { id: "resolve_and_reintegrate", label: "Host resolution" }, { id: "stop_mission", label: "Host stop" },
      ] });
      const next = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      const decision = next.entities.flatMap(e => "Decision" in e && e.Decision.id === decisionId ? [e.Decision] : [])[0];
      const failed = { ...run, state: "failed" as const, binding_snapshot: null, requested_model: null,
        observed_model: null, ended_at: "2026-09-16T00:00:00Z" };
      client.seedMissionEntities(missionId, [
        { Mission: { ...mission, phase: "integrating", open_decision_count: 1, policy: { ...mission.policy, allowed_binding_ids: [binding.id] } } },
        { Task: { ...task, title: "작업 결과 통합", kind: "integrate", role: "integrator", binding_id: binding.id,
          state: "failed", active_run_id: null, integration: { plan_ref: task.contract.objective_ref, step: "automatic" } } },
        { Run: failed },
        { Decision: { ...decision, kind: "conflict", requesting_run_id: run.id, affected_task_ids: [task.id] } },
      ]);
      return { taskId: task.id, run: failed };
    }, id);
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    await page.getByTestId("toggle-detail").click();
    await page.getByTestId("team-row").first().click();
    await expect(page.getByTestId("integration-assignment")).toContainText("통합 담당 모델 · integrator-model");
    await expect(page.getByTestId("detail-model")).toHaveText("없음");
    await expect(page.getByTestId("task-retry")).toBeDisabled();
    await expect(page.getByTestId("task-reassign")).toBeEnabled();
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("decision-banner-text")).toContainText("통합 충돌의 해결 방법을 선택하세요.");
    await expect(page.getByTestId("decision-banner-text")).not.toContainText("integration_conflict");
    await page.getByTestId("decision-banner-open").click();
    const panel = page.getByTestId("decision-panel");
    await expect(panel).toContainText("같은 작업 공간에서 해결한 뒤");
    await expect(panel.getByTestId("integration-conflict-paths")).toContainText("src/shared.ts");
    const resolve = panel.getByRole("button", { name: "Integrator에게 해결 요청", exact: true });
    await resolve.scrollIntoViewIfNeeded();
    await expect(resolve).toBeInViewport({ ratio: 0.99 });
    const width = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(width.scroll).toBeLessThanOrEqual(width.width + 1);
    await page.screenshot({ path: testInfo.outputPath(`integration-conflict-${layout}.png`) });
    await resolve.click();
    await expect(resolve).toBeDisabled();
    const after = await page.evaluate(async missionId => {
      const client = window.__mockDaemon!.client as MockDaemonClient;
      return (await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null })).entities;
    }, id);
    expect(after.flatMap(e => "Run" in e ? [e.Run] : [])).toEqual([prior.run]);
    expect(after.flatMap(e => "Task" in e ? [e.Task] : [])[0]).toMatchObject({ id: prior.taskId,
      state: "ready", integration: { step: { resolving: { conflict_run_id: prior.run.id } } } });
  });
}
