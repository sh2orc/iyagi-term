import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`충돌한 변경 제외 후 필수 재계획을 ${layout} 화면에서 확인한다`, async ({ page }, testInfo) => {
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
      const decisionId = await api.openDecision(missionId, { question: JSON.stringify({ phase: "integration_conflict", conflict: { paths: ["src/shared.ts"] } }), blocking: true, options: [
        { id: "resolve_and_reintegrate", label: "Host resolution" }, { id: "exclude_candidate", label: "Host exclusion" }, { id: "stop_mission", label: "Host stop" },
      ] });
      const next = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      const decision = next.entities.flatMap(e => "Decision" in e && e.Decision.id === decisionId ? [e.Decision] : [])[0];
      const failed = { ...run, state: "failed" as const, binding_snapshot: null, requested_model: null,
        observed_model: null, ended_at: "2026-09-16T00:00:00Z" };
      const bindingId = crypto.randomUUID();
      client.seedMissionEntities(missionId, [
        { Mission: { ...mission, phase: "integrating", open_decision_count: 1,
          policy: { ...mission.policy, allowed_binding_ids: [bindingId] },
          role_bindings: [{ role: "lead", primary_binding_id: bindingId, fallback_binding_ids: [] }] } },
        { Task: { ...task, title: "작업 결과 통합", kind: "integrate", role: "integrator", binding_id: bindingId,
          state: "failed", active_run_id: null, integration: { plan_ref: task.contract.objective_ref, step: "automatic" } } },
        { Run: failed },
        { Decision: { ...decision, kind: "conflict", requesting_run_id: run.id, affected_task_ids: [task.id] } },
      ]);
      return { taskId: task.id, run: failed, requirements: mission.requirements, bindingId };
    }, id);
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    await page.getByTestId("decision-banner-open").click();
    const panel = page.getByTestId("decision-panel");
    await expect(panel.getByTestId("integration-exclusion-note")).toContainText("기존 기록과 파일은 보존");
    const exclude = panel.getByRole("button", { name: "충돌한 변경 제외 후 Lead 재계획", exact: true });
    await exclude.scrollIntoViewIfNeeded();
    await expect(exclude).toBeInViewport({ ratio: 0.99 });
    const width = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(width.scroll).toBeLessThanOrEqual(width.width + 1);
    await page.screenshot({ path: testInfo.outputPath(`candidate-exclusion-${layout}.png`) });
    await exclude.click();
    await expect(exclude).toBeDisabled();
    const after = await page.evaluate(async missionId => {
      const client = window.__mockDaemon!.client as MockDaemonClient;
      return (await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null })).entities;
    }, id);
    expect(after.flatMap(e => "Run" in e ? [e.Run] : [])).toEqual([prior.run]);
    const tasks = after.flatMap(e => "Task" in e ? [e.Task] : []);
    expect(tasks.find(t => t.id === prior.taskId)?.state).toBe("superseded");
    expect(tasks.find(t => t.kind === "plan")).toMatchObject({ required: true, state: "planned", role: "lead", binding_id: prior.bindingId });
    expect(after.flatMap(e => "Mission" in e ? [e.Mission] : [])[0]).toMatchObject({ phase: "planning", candidate_id: null, requirements: prior.requirements });
  });
}
