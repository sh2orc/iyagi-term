/**
 * U03 — 팀 1→4→12 성장: 새 탭 생성/포커스 훔치기 없음(activeTabId 불변).
 * U05 — task 클릭 후에도 composer 수신자는 Lead 고정.
 * U06 — `이 할 일에 추가 지시` 고정 수신자 + 별도 draft.
 * U07 — queued/unknown 전달 상태의 정직한 문구.
 */

import { expect, test } from "@playwright/test";
import {
  harnessAddAgents,
  harnessAddMessage,
  harnessGetActiveTabId,
  harnessGetTabCount,
  prepare,
  seedAndOpen,
} from "./helpers";

test.beforeEach(async ({ page }) => {
  await prepare(page);
});

test("U03: 작업이 1→4→12로 늘어나도 탭을 만들거나 포커스를 훔치지 않는다", async ({ page }) => {
  const missionId = await seedAndOpen(page, { agents: 1 });
  await expect(page.getByTestId("team-row")).toHaveCount(1);

  const beforeTabCount = await harnessGetTabCount(page);
  const activeBefore = await harnessGetActiveTabId(page);

  await harnessAddAgents(page, missionId, 3);
  await expect(page.getByTestId("team-row")).toHaveCount(4);

  await harnessAddAgents(page, missionId, 8);
  await expect(page.getByTestId("team-row")).toHaveCount(12);

  expect(await harnessGetActiveTabId(page)).toBe(activeBefore);
  expect(await harnessGetTabCount(page)).toBe(beforeTabCount);
});

test("U05: task 클릭은 실행을 표시하고 Lead 대화는 별도로 연다", async ({ page }) => {
  await seedAndOpen(page, { agents: 3 });
  await page.getByTestId("team-row").first().click();
  await expect(page.getByTestId("run-detail")).toBeVisible();
  await page.getByTestId("toggle-lead").click();
  await expect(page.getByTestId("lead-recipient")).toHaveText("AI 작업 전체에 요청");
});

test("U06: 이 할 일에 추가 지시 — 수신자 고정 + Lead draft와 분리", async ({ page }) => {
  await seedAndOpen(page, { agents: 2 });
  await page.getByTestId("team-row").first().click();
  await expect(page.getByTestId("run-detail")).toBeVisible();

  await page.getByTestId("instruct-toggle").click();
  const taskComposer = page.getByTestId("task-composer");
  await expect(taskComposer).toBeVisible();
  // 수신자 표기에는 task명과 모델이 고정된다.
  await expect(page.getByTestId("task-recipient")).toContainText("GLM-5.3");

  await page.getByTestId("task-composer-input").fill("이 부분 다시 확인해 줘");
  await expect(page.getByTestId("task-composer-input")).toHaveValue("이 부분 다시 확인해 줘");
  // Lead composer의 draft는 별개다(05 §4).
  await page.getByTestId("toggle-lead").click();
  await expect(page.getByTestId("lead-composer")).toHaveValue("");
});

test("U07: queued/unknown 전달 상태를 정직하게 표시한다(queued ≠ delivered)", async ({ page }) => {
  const missionId = await seedAndOpen(page, { agents: 1 });
  await harnessAddMessage(page, missionId, { role: "user", text: "이 지시는 아직 대기 중", delivery: "queued" });
  await harnessAddMessage(page, missionId, { role: "user", text: "이 지시는 전달 여부 확인 필요", delivery: "unknown" });

  await page.getByTestId("toggle-lead").click();
  const lead = page.getByTestId("lead-scroll");
  await expect(lead).toContainText("전달 여부 확인 필요", { timeout: 10_000 });
  await expect(lead).toContainText("이 지시는 아직 대기 중");
  // goal 사용자 메시지(delivered) 1건만 '전달됨'이어야 한다 — queued를
  // delivered로 속여 표시하지 않는다.
  const deliveredCount = await page.evaluate(() => {
    const text = document.querySelector("[data-testid='lead-scroll']")?.textContent ?? "";
    return text.split("전달됨").length - 1;
  });
  expect(deliveredCount).toBe(1);
});

test("U06: 두 입력창의 메시지를 전송하고 갱신된 대화를 계속 사용할 수 있다", async ({ page }) => {
  await seedAndOpen(page, { agents: 2 });
  await page.getByTestId("toggle-lead").click();
  await page.getByTestId("lead-composer").fill("전체 계획을 다시 확인해 줘");
  await page.getByTestId("team-row").first().click();
  await page.getByTestId("instruct-toggle").click();
  await page.getByTestId("task-composer-input").fill("이 담당은 오류 경계를 확인해 줘");
  await page.getByTestId("task-composer-send").click();
  await expect(page.getByTestId("task-composer-input")).toHaveValue("");
  await page.getByTestId("toggle-lead").click();
  await expect(page.getByTestId("lead-composer")).toHaveValue("전체 계획을 다시 확인해 줘");
  await expect(page.getByTestId("lead-scroll")).toContainText("이 담당은 오류 경계를 확인해 줘");
  await page.getByTestId("lead-send").click();
  await page.getByTestId("toggle-lead").click();
  await expect(page.getByTestId("lead-composer")).toHaveValue("");
  await expect(page.getByTestId("lead-scroll")).toContainText("전체 계획을 다시 확인해 줘");
  await page.getByTestId("toggle-lead").click();
  await expect(page.getByTestId("lead-recipient")).toHaveText("AI 작업 전체에 요청");
});
