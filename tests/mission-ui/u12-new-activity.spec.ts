/**
 * U12 — 사용자가 위를 읽는 동안 tail-follow를 끊고 `새 활동 N개`만 보여 준다.
 */

import { expect, test } from "@playwright/test";
import { harnessAddMessage, prepare, seedAndOpen } from "./helpers";

test.beforeEach(async ({ page }) => {
  await prepare(page);
});

test("U12: 스크롤을 올린 채 새 메시지가 오면 새 활동 버튼만 뜬다", async ({ page }) => {
  // 스크롤이 생길 만큼 낮은 높이 + 충분한 보고.
  await page.setViewportSize({ width: 1440, height: 480 });
  const missionId = await seedAndOpen(page, { agents: 1 });

  await page.getByTestId("toggle-lead").click();
  for (let i = 0; i < 10; i += 1) {
    await harnessAddMessage(page, missionId, { role: "agent", text: `진행 보고 ${i + 1}` });
  }
  await expect(page.getByTestId("lead-scroll")).toContainText("진행 보고 10", { timeout: 10_000 });
  await expect
    .poll(
      async () =>
        page.getByTestId("lead-scroll").evaluate((el) => (el as HTMLElement).scrollHeight - (el as HTMLElement).clientHeight),
      { timeout: 10_000 },
    )
    .toBeGreaterThan(48);

  await page.getByTestId("lead-scroll").evaluate((el) => {
    (el as HTMLElement).scrollTop = 0;
    (el as HTMLElement).dispatchEvent(new Event("scroll"));
  });

  await harnessAddMessage(page, missionId, { role: "agent", text: "위로 벗어난 사이 새 보고" });

  const button = page.getByTestId("new-activity");
  await expect(button).toBeVisible({ timeout: 10_000 });
  await expect(button).toContainText("새 활동 1개");

  // 버튼을 누르면 끝으로 돌아가 follow가 재개된다.
  await button.click();
  await expect(page.getByTestId("new-activity")).toHaveCount(0);
  await expect(page.getByTestId("lead-scroll")).toContainText("위로 벗어난 사이 새 보고");
});
