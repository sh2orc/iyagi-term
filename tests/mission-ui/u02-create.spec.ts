/**
 * U02 — 새 AI 작업 대화상자: goal upload → mission.create → start.
 * start 실패 → 오류 + 재시도(같은 mission, 중복 생성 없음).
 *
 * 팀은 설정 → AI 작업의 빠른 설정으로 만든다(Codex 카드 · 검증 모델 gpt-5.6-luna).
 * mock probe가 이 조합을 데몬 registry처럼 검증해 네 역할이 모두 준비된다.
 * 수동 연결 폼 경로는 u07/u29가 다룬다.
 */

import { expect, test, type Locator, type Page } from "@playwright/test";
import { harnessFailNextStart, harnessMissions, prepare } from "./helpers";

const QUICK_TEAM = "Codex · gpt-5.6-luna";

test.beforeEach(async ({ page }) => {
  await prepare(page);
  await page.getByRole("button", { name: "설정", exact: true }).click();
  await page.locator(".settings-nav button").filter({hasText: /^AI 작업/}).click();
  const settings=page.locator(".mission-settings");
  const quick=settings.getByTestId("mission-quick-setup");
  const codex=quick.locator("[data-runtime=codex]");
  await expect(codex.locator(".mission-quick-setup-model select")).toHaveValue("gpt-5.6-luna");
  await codex.getByRole("button",{name:"이 모델로 팀 만들기",exact:true}).click();
  await expect(codex.locator(".mission-quick-setup-status")).toContainText(`팀 ‘${QUICK_TEAM}’ 준비 완료`);
  await expect(settings.locator("[data-setting-id=missionTeams] li").first()).toContainText(QUICK_TEAM);
  await settings.getByTestId("mission-guide").locator("summary").click();
  await expect(settings.getByTestId("mission-guide")).toContainText("Lead가 할 일을 나누고");
  await page.screenshot({path:"tests/mission-ui/__results/mission-settings.png",fullPage:true});
  await page.getByRole("button",{name:"터미널로 이동",exact:true}).click();
});

/** 새 대화상자는 첫 팀을 골라 두고, 완료 조건은 접혀 있다. */
async function fillDialog(page: Page, goal: string): Promise<Locator> {
  const dialog = page.getByTestId("mission-create");
  await expect(dialog).toBeVisible();
  await expect(page.getByTestId("mission-create-sidebar")).toBeVisible();
  await expect(page.locator(".modal-backdrop")).toHaveCount(0);
  await expect(dialog.getByTestId("create-team").locator("option:checked")).toHaveText(QUICK_TEAM);
  await dialog.getByTestId("create-goal").fill(goal);
  return dialog;
}

async function fillRepositoryAndRequirement(dialog: Locator): Promise<void> {
  const location = dialog.getByTestId("create-location");
  if (!(await location.evaluate((element) => (element as HTMLDetailsElement).open))) await location.locator("summary").click();
  await dialog.getByTestId("create-repo").fill("/repo");
  // 완료 조건은 선택 사항이라 접혀 있다 — 펼친 뒤 적는다.
  await dialog.getByTestId("create-requirements").locator("summary").click();
  await dialog.getByTestId("create-req-0").fill("The requested behavior works.");
  await expect(dialog.getByTestId("create-repo-status")).toContainText("/repo");
  await expect(dialog.getByTestId("create-submit")).toBeEnabled();
  await expect(dialog.getByTestId("create-submit-reason")).toHaveCount(0);
}

test("U02: 상단 새 AI 작업 버튼으로 mission을 만들고 시작한다", async ({ page }) => {
  await page.getByTestId("new-mission-button").click();

  const dialog = await fillDialog(page, "로그인 기능을 구현해 주세요.");
  await fillRepositoryAndRequirement(dialog);
  await dialog.getByTestId("create-submit").click();

  await expect(dialog).toBeHidden();
  await expect(page.getByTestId("mission-page")).toBeVisible();
  await expect.poll(async () => (await harnessMissions(page))[0]?.state).toBe("running");
  const missions = await harnessMissions(page);
  expect(missions.length).toBe(1);
  expect(missions[0].state).toBe("running");
  // goal artifact가 Lead 대화의 사용자 목표로 나타난다.
  await page.getByTestId("toggle-lead").click();
  const lead = page.getByTestId("lead-scroll");
  await expect(lead).toContainText("로그인 기능을 구현해 주세요.");
});

test("U02: start 실패는 오류+재시도를 표시하고 중복 mission을 만들지 않는다", async ({ page }) => {
  await page.getByTestId("new-mission-button").click();
  const dialog = await fillDialog(page, "결제 리팩터링");
  await harnessFailNextStart(page);
  await fillRepositoryAndRequirement(dialog);
  await dialog.getByTestId("create-submit").click();

  await expect(dialog).toBeHidden();
  await expect(page.getByTestId("mission-control-error")).toBeVisible();
  let missions = await harnessMissions(page);
  expect(missions.length).toBe(1);
  expect(missions[0].state).toBe("draft");

  await page.getByTestId("mission-start").click();
  await expect.poll(async () => (await harnessMissions(page))[0]?.state).toBe("running");
  await expect(dialog).toBeHidden();
  await expect(page.getByTestId("mission-page")).toBeVisible();
  missions = await harnessMissions(page);
  expect(missions.length).toBe(1);
  expect(missions[0].state).toBe("running");
});

for (const [label, width, zoom] of [["700", 700, 1], ["zoom200", 1440, 2]] as const) {
  test(`AI 작업 설정과 사용법은 ${label} 화면에서도 넘치지 않는다`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.getByRole("button", { name: "설정", exact: true }).click();
    await page.locator(".settings-nav button").filter({ hasText: /^AI 작업/ }).click();
    await page.evaluate((value) => { document.documentElement.style.zoom = String(value); }, zoom);
    const settings = page.locator(".mission-settings");
    await expect(settings.getByTestId("mission-guide")).toBeVisible();
    await settings.getByTestId("mission-guide").locator("summary").click();
    const overflow = await settings.evaluate((node) => ({ width: node.clientWidth, scroll: node.scrollWidth }));
    expect(overflow.scroll).toBeLessThanOrEqual(overflow.width + 1);
    await page.screenshot({ path: `tests/mission-ui/__results/mission-settings-${label}.png`, fullPage: false });
    await settings.getByRole("button", { name: "팀 저장", exact: true }).scrollIntoViewIfNeeded();
    await expect(settings.getByRole("button", { name: "팀 저장", exact: true })).toBeVisible();
  });
}
