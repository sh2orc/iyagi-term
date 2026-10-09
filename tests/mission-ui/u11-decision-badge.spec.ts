/**
 * U11 — 다른 탭에서 결정이 열리면 badge만 갱신한다: 강제 탭 전환·모달 금지.
 */

import { expect, test } from "@playwright/test";
import {
  harnessGetActiveTabId,
  harnessOpenDecision,
  prepare,
  seedAndOpen,
} from "./helpers";

test.beforeEach(async ({ page }) => {
  await prepare(page);
});

test("U11: 터미널 탭을 보는 중 결정이 오면 mission 탭 배지만 바뀐다", async ({ page }) => {
  const missionId = await seedAndOpen(page, { agents: 2 });

  // 첫 번째 탭(자동 시작된 터미널)으로 이동한다.
  const terminalTab = page.locator('[role="tab"]').first();
  await terminalTab.click();
  const activeBefore = await harnessGetActiveTabId(page);

  // mission 탭(두 번째)의 배지 — 상태 우선 한 개. 결정이 생기면 "결정 필요 N"(경고 스타일)으로 바뀐다.
  const missionTab = page.locator('[role="tab"]').nth(1);
  await expect(missionTab.locator(".tab-badge-mission-decision")).toHaveCount(0);

  await harnessOpenDecision(page, missionId, { blocking: true });

  const decisionBadge = missionTab.locator(".tab-badge-mission-decision");
  await expect(decisionBadge).toContainText("결정 필요 1", { timeout: 10_000 });
  // 배지는 낭독 영역이 아니다(탭마다 role="status"면 갱신마다 읽힌다).
  await expect(decisionBadge).not.toHaveAttribute("role", "status");
  // 탭 전환·모달·배너 강제 노출이 없어야 한다(05 §8).
  expect(await harnessGetActiveTabId(page)).toBe(activeBefore);
  await expect(page.locator('[role="dialog"]')).toHaveCount(0);
  await expect(page.getByTestId("decision-banner")).toHaveCount(0);
  await expect(page.getByTestId("mission-page")).toHaveCount(0);
});
