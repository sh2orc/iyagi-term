import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  for (const integration of [false, true]) {
  test(`${integration ? "통합 재구성" : "종료 확인 후 새 시도"} 안내를 ${layout} 화면에서 읽고 선택한다`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const id = await seedAndOpen(page, { agents: 1 });
    const before = await page.evaluate(async ({ missionId, integration }) => {
      const api = window.__mockDaemon!;
      const client = api.client as MockDaemonClient;
      const snapshot = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      const task = snapshot.entities.flatMap(e => "Task" in e ? [e.Task] : [])[0];
      if (integration) {
        task.kind = "integrate"; task.role = "integrator";
        task.integration = { plan_ref: task.contract.objective_ref, step: { resolving: { conflict_run_id: "old-conflict" } } };
      }
      const run = snapshot.entities.flatMap(e => "Run" in e ? [e.Run] : [])[0];
      const decisionId = await api.openDecision(missionId, { question: "Fixture local termination proof", blocking: false, options: [
        { id: "retry_reconciled_task", label: "Host retry" }, { id: "stop_reconciled_mission", label: "Host stop" },
      ] });
      const next = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      const decision = next.entities.flatMap(e => "Decision" in e && e.Decision.id === decisionId ? [e.Decision] : [])[0];
      const execId = `fixture-exec-${run.id}`;
      const unknown = { ...run, exec_id: execId, state: "unknown" as const, reconciliation_ref: run.context_ref, failure_code: "OUTCOME_UNKNOWN" as const };
      client.seedMissionEntities(missionId, [
        { Exec: { id: execId, mission_id: missionId, run_id: run.id, state: "exited", identity: null,
          group_kind: "observed_tree", group_reference: "fixture-observer", group_identity: null,
          resource_policy: { reservation_bytes: "1", cpu_slots: 1, enforcement: "observe", memory_max_bytes: null, cpu_max_cores: null, pids_max: null },
          launch_manifest_ref: run.context_ref, owner_daemon_id: "fixture-owner", started_at: run.started_at, ended_at: "2026-09-16T00:00:00Z", exit_code: null } },
        { Task: { ...task, state: "blocked", active_run_id: null, workspace_id: null, blocked_code: "outcome_unknown_ended" } },
        { Run: unknown },
        { Decision: { ...decision, kind: "recovery", requesting_run_id: run.id, affected_task_ids: [task.id] } },
      ]);
      return { taskId: task.id, unknown, workspaces: snapshot.entities.filter(e => "Workspace" in e) };
    }, { missionId: id, integration });
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    await expect(page.getByTestId("decision-banner-text")).toContainText("실행 종료 확인");
    await page.getByTestId("toggle-detail").click();
    await page.getByTestId("team-row").first().click();
    const notice = page.getByTestId("recovery-notice");
    await expect(notice).toContainText("로컬 실행의 종료를 확인했습니다");
    await notice.scrollIntoViewIfNeeded();
    await expect(notice).toBeInViewport({ ratio: 0.99 });
    await expect(page.getByTestId("task-retry")).toHaveCount(0);
    await expect(page.getByTestId("task-reassign")).toBeEnabled();
    await page.keyboard.press("Escape");
    await page.getByTestId("decision-banner-open").click();
    const panel = page.getByTestId("decision-panel");
    await expect(panel).toContainText(integration ? "이전 파일은 격리해 둡니다" : "제공자 결과와 외부 영향은 알 수 없으니");
    await expect(panel.getByTestId("recovery-coverage-notice")).toContainText("자식 프로세스는 남아 있을 수 있습니다");
    const retry = panel.getByRole("button", { name: integration ? "격리된 결과 보존하고 통합 다시 구성" : "이전 영향 확인 후 새 시도 시작", exact: true });
    await retry.scrollIntoViewIfNeeded();
    await expect(retry).toBeInViewport({ ratio: 0.99 });
    await expect(panel.getByRole("button", { name: "AI 작업 중단", exact: true })).toBeVisible();
    const width = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(width.scroll).toBeLessThanOrEqual(width.width + 1);
    await page.screenshot({ path: testInfo.outputPath(`${integration ? "integration-rebuild" : "reconciliation"}-${layout}.png`) });
    await retry.click();
    await expect(panel).toContainText("답변됨");
    await expect(retry).toBeDisabled();
    if (integration) {
      const after = await page.evaluate(async missionId => (await window.__mockDaemon!.client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null })).entities, id);
      expect(after.flatMap(e => "Task" in e && e.Task.id === before.taskId ? [e.Task] : [])[0]).toMatchObject({ state: "ready", workspace_id: null, integration: { step: "automatic" } });
      expect(after.flatMap(e => "Run" in e && e.Run.id === before.unknown.id ? [e.Run] : [])[0]).toEqual(before.unknown);
      expect(after.filter(e => "Workspace" in e)).toEqual(before.workspaces);
    }
  });
  }
}
