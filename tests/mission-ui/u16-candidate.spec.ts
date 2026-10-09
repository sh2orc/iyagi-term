/**
 * U16 — 결과 확정: 직접 확인을 모두 체크해야 확정 버튼이 열리고, 후보가 바뀌면
 * disable + 갱신 요구, 갱신 후에는 새 후보 기준으로 다시 확인한다.
 */

import { expect, test, type Locator } from "@playwright/test";
import {
  harnessReplaceCandidate,
  harnessStageForAcceptance,
  prepare,
  seedAndOpen,
} from "./helpers";

test.beforeEach(async ({ page }) => {
  await prepare(page);
});

async function checkAll(section: Locator): Promise<void> {
  for (const box of await section.getByRole("checkbox").all()) {
    if (!(await box.isDisabled()) && !(await box.isChecked())) await box.check();
  }
}

test("U16: 직접 확인 후 확정 가능, 검토 중 후보가 교체되면 disable되고 갱신 후 다시 확인한다", async ({ page }) => {
  const missionId = await seedAndOpen(page, { agents: 2 });
  await harnessStageForAcceptance(page, missionId);

  // 확정 대기 → 오른쪽 아래가 결과 화면으로 바뀐다.
  const review = page.getByTestId("result-review");
  await expect(review).toBeVisible({ timeout: 10_000 });
  const accept = page.getByTestId("accept-button");
  await expect(page.getByTestId("result-status")).toContainText("확정 대기");
  await expect(accept).toBeDisabled();
  await expect(page.getByTestId("accept-blocked-reason")).toBeVisible();
  await expect(page.getByTestId("result-file")).toHaveCount(2);

  const confirmations = page.getByTestId("human-confirmations");
  await checkAll(confirmations);
  await expect(accept).toBeEnabled();

  // daemon 쪽에서 후보가 교체되었다(05 §9).
  await harnessReplaceCandidate(page, missionId);
  await expect(accept).toBeDisabled({ timeout: 10_000 });
  await expect(page.getByTestId("candidate-changed")).toContainText("결과가 바뀌었습니다");

  // 갱신하면 새 후보로 다시 binding되고, 확인 체크는 새 후보 기준으로 다시 받는다.
  await page.getByTestId("candidate-changed").locator("button").click();
  await expect(page.getByTestId("candidate-changed")).toHaveCount(0);
  await expect(accept).toBeDisabled();
  await checkAll(confirmations);
  await expect(accept).toBeEnabled({ timeout: 10_000 });
});
