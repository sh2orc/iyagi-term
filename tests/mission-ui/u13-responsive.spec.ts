/**
 * U13 — 반응형: 1440/1024/700px + 200% zoom에서 요소가 겹치지 않는다
 * (가로 넘침 없음 + 스크린샷 아티팩트).
 */

import { expect, test } from "@playwright/test";
import { prepare, seedAndOpen } from "./helpers";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SHOT_DIR = path.join(path.dirname(fileURLToPath(import.meta.url)), "__screenshots__");

async function assertNoHorizontalOverflow(page: import("@playwright/test").Page): Promise<void> {
  const overflow = await page.evaluate(() => {
    const doc = document.scrollingElement ?? document.documentElement;
    return { scrollWidth: doc.scrollWidth, clientWidth: doc.clientWidth };
  });
  expect(overflow.scrollWidth).toBeLessThanOrEqual(overflow.clientWidth + 1);
}

test.beforeEach(async ({ page }) => {
  await prepare(page);
});

test("U13: 1440px wide 레이아웃 — 실행 본문과 팀이 나란히 있다", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await seedAndOpen(page, { agents: 3 });
  await expect(page.getByTestId("mission-page")).toHaveAttribute("data-mode", "wide");
  await expect(page.getByTestId("team-list")).toBeVisible();
  await expect(page.getByTestId("team-list")).toBeVisible();
  await assertNoHorizontalOverflow(page);
  await page.screenshot({ path: path.join(SHOT_DIR, "mission-1440.png"), fullPage: false });
});

test("U13: 1024px 중간 폭 — 목록 + 실행 본문", async ({ page }) => {
  await page.setViewportSize({ width: 1024, height: 800 });
  await seedAndOpen(page, { agents: 3 });
  await expect(page.getByTestId("mission-page")).toHaveAttribute("data-mode", "medium");
  await expect(page.getByTestId("team-list")).toBeVisible();
  await expect(page.getByTestId("team-list")).toBeVisible();
  // 작업 선택은 본문을 바꾸며 별도 팝업을 만들지 않는다.
  await expect(page.getByTestId("side-sheet")).toHaveCount(0);
  await page.getByTestId("team-row").first().click();
  await expect(page.getByTestId("side-sheet")).toHaveCount(0);
  await expect(page.getByTestId("run-detail")).toHaveCount(1);
  await assertNoHorizontalOverflow(page);
  await page.screenshot({ path: path.join(SHOT_DIR, "mission-1024.png"), fullPage: false });
});

test("U13: 700px 좁은 폭 — 한 영역만 표시 + 탐색", async ({ page }) => {
  await page.setViewportSize({ width: 700, height: 800 });
  await seedAndOpen(page, { agents: 2 });
  await expect(page.getByTestId("mission-page")).toHaveAttribute("data-mode", "narrow");
  await expect(page.getByTestId("toggle-lead")).toBeVisible();
  await expect(page.getByTestId("toggle-detail")).toBeVisible();
  await expect(page.getByTestId("toggle-result")).toBeVisible();
  // 기본 영역은 팀 목록이며 선택하면 본문으로 이동한다.
  await expect(page.getByTestId("team-list")).toBeVisible();
  await page.getByTestId("team-row").first().click();
  await expect(page.getByTestId("run-detail")).toBeVisible();
  await expect(page.getByTestId("team-list")).toHaveCount(0);
  await page.getByTestId("toggle-detail").click();
  await expect(page.getByTestId("team-list")).toBeVisible();
  await expect(page.getByTestId("lead-composer")).toHaveCount(0);
  await assertNoHorizontalOverflow(page);
  await page.screenshot({ path: path.join(SHOT_DIR, "mission-700.png"), fullPage: false });
});

test("U13: 200% zoom — 가로 넘침/겹침 없음", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await seedAndOpen(page, { agents: 3 });
  await page.evaluate(() => {
    document.documentElement.style.zoom = "2";
  });
  await page.waitForTimeout(300);
  // zoom 200%는 실효 폭이 절반(≈720px) — 좁은 폭 레이아웃으로 전환해 한
  // 영역만 표시한다(겹침 원천 차단). 탐색으로 영역을 바꿔도 넘치지 않는다.
  await expect(page.getByTestId("mission-page")).toHaveAttribute("data-mode", "narrow", { timeout: 10_000 });
  await expect(page.getByTestId("team-list")).toBeVisible();
  await assertNoHorizontalOverflow(page);
  await page.getByTestId("toggle-detail").click();
  await expect(page.getByTestId("team-row").first()).toBeVisible();
  await assertNoHorizontalOverflow(page);
  await page.screenshot({ path: path.join(SHOT_DIR, "mission-1440-zoom200.png"), fullPage: false });
});
