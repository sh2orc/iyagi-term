import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`필수 작업 실패와 Lead 수정 계획을 ${layout} 화면에서 연결한다`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const id = await seedAndOpen(page, { agents: 1 });
    await page.evaluate(async missionId => {
      const client = window.__mockDaemon!.client as MockDaemonClient;
      const snapshot = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      const task = snapshot.entities.flatMap(e => "Task" in e ? [e.Task] : [])[0];
      const run = snapshot.entities.flatMap(e => "Run" in e ? [e.Run] : [])[0];
      client.seedMissionEntities(missionId, [
        { Task: { ...task, state: "failed", active_run_id: null, attempt_count: 3 } },
        { Run: { ...run, state: "failed", attempt: 3, failure_code: "PROVIDER_UNAVAILABLE", ended_at: new Date().toISOString() } },
        { Task: { ...task, id: crypto.randomUUID(), title: "필수 할 일 수정 계획", kind: "plan", role: "lead", state: "ready", active_run_id: null, attempt_count: 0, repair_cycle: 1, failure_repair_run_ids: [run.id], ordinal: 99, workspace_id: null } },
      ]);
    }, id);
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    await page.getByTestId("toggle-detail").click();
    await page.getByTestId("team-row").first().click();
    await expect(page.getByTestId("required-repair-notice")).toContainText("Lead가 다시 계획합니다");
    await expect(page.getByTestId("task-retry")).toBeDisabled();
    await expect(page.getByTestId("task-reassign")).toBeDisabled();
    await page.keyboard.press("Escape");
    await page.getByTestId("team-row").filter({ hasText: "필수 할 일 수정 계획" }).click();
    const notice = page.getByTestId("required-repair-notice");
    await expect(notice).toContainText("관련 없는 할 일은 계속 진행");
    await notice.scrollIntoViewIfNeeded();
    await expect(notice).toBeInViewport({ ratio: 0.99 });
    const reassign = page.getByTestId("task-reassign");
    await expect(reassign).toBeEnabled();
    await reassign.scrollIntoViewIfNeeded();
    await expect(reassign).toBeInViewport({ ratio: 0.99 });
    const size = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(size.scroll).toBeLessThanOrEqual(size.width + 1);
    await page.screenshot({ path: testInfo.outputPath(`required-repair-${layout}.png`) });
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("side-sheet")).toHaveCount(0);
    await expect(page.getByTestId("team-row").filter({ hasText: "필수 할 일 수정 계획" })).toBeFocused();
  });
}

for (const layout of ["700", "zoom200"] as const) {
  test(`리뷰 수정 한도의 사용자 판단 안내를 ${layout} 화면에서 읽는다`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const missionId = await seedAndOpen(page, { agents: 1 });
    await page.evaluate(async id => {
      const api = window.__mockDaemon!, client = api.client as MockDaemonClient;
      await api.stageForAcceptance(id);
      const decisionId = await api.openDecision(id, { question: "Review repair budget fixture", options: [{ id: "stop_review_repair", label: "Host stop" }] });
      const snapshot = await client.missionSnapshot({ mission_id: id, snapshot_id: null, cursor: null });
      const decision = snapshot.entities.flatMap(e => "Decision" in e && e.Decision.id === decisionId ? [e.Decision] : [])[0];
      const mission = snapshot.entities.flatMap(e => "Mission" in e ? [e.Mission] : [])[0];
      const candidate = snapshot.entities.flatMap(e => "Candidate" in e ? [e.Candidate] : [])[0];
      client.seedMissionEntities(id, [
        { Mission: { ...mission, phase: "reviewing" } },
        { Finding: { id: crypto.randomUUID(), mission_id: id, candidate_id: candidate.id, reviewer_run_id: candidate.source_run_ids[0] ?? crypto.randomUUID(), severity: "major", path: "api.txt", line: 1, evidence_ref: mission.goal_ref, requirement_id: null, resolution: "open", resolution_ref: null } },
        { Decision: { ...decision, kind: "budget", blocking: true, affected_task_ids: [], requesting_run_id: null } },
      ]);
    }, missionId);
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    await page.getByTestId("decision-banner-open").click();
    const question = page.getByTestId("decision-question");
    await expect(question).toContainText("리뷰 자동 수정 한도에 도달했습니다");
    // 긴 안내(근거 해제·한도 변경·중단)는 자세히 안에 있다.
    await expect(page.getByTestId("decision-details")).toContainText("사유와 함께 해제");
    await question.scrollIntoViewIfNeeded();
    await expect(question).toBeInViewport({ ratio: 0.99 });
    const stop = page.getByTestId("decision-option");
    await expect(stop).toBeEnabled();
    await expect(stop).toHaveText("AI 작업 중단");
    await stop.scrollIntoViewIfNeeded();
    await expect(stop).toBeInViewport({ ratio: 0.99 });
    const size = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(size.scroll).toBeLessThanOrEqual(size.width + 1);
    await page.screenshot({ path: testInfo.outputPath(`review-repair-${layout}.png`) });
    await page.getByRole("button", { name: "결과", exact: true }).click();
    const finding = page.getByTestId("finding-review");
    await expect(finding).toContainText("api.txt:1");
    const reason = finding.getByTestId("finding-reason");
    await reason.fill("현재 구현과 검증 기록을 확인했습니다. 이 동작은 기존 공개 계약에 필요하므로 지적을 해제합니다.");
    await reason.scrollIntoViewIfNeeded();
    await expect(reason).toBeInViewport({ ratio: 0.99 });
    await page.screenshot({ path: testInfo.outputPath(`review-input-${layout}.png`) });
    const dismiss = finding.getByTestId("finding-dismiss");
    await dismiss.scrollIntoViewIfNeeded();
    await expect(dismiss).toBeInViewport({ ratio: 0.99 });
    await page.screenshot({ path: testInfo.outputPath(`review-reason-${layout}.png`) });
    await dismiss.click();
    await expect(finding.getByTestId("finding-reason")).toHaveCount(0);
    await expect(finding).toContainText("저장된 해제 사유");
    await expect(finding).toContainText("기존 공개 계약에 필요");
    const saved = await page.evaluate(async id => {
      const snapshot = await window.__mockDaemon!.client.missionSnapshot({ mission_id: id, snapshot_id: null, cursor: null });
      return snapshot.entities.flatMap(e => "Finding" in e ? [e.Finding] : [])[0];
    }, missionId);
    expect(saved.resolution).toBe("dismissed");
    expect(saved.resolution_ref).not.toBeNull();
  });
}
