/**
 * U04 — IME 조합 중 Enter는 전송하지 않는다(실제 브라우저 키 입력).
 * 조합은 CDP Input.imeSetComposition으로 실제 IME 상태를 만들고, 그다음
 * Enter가 보내지 않는지(전송 횟수 0) 확인 후 명시적 전송 1회만 단언한다.
 */

import { expect, test } from "@playwright/test";
import { instrumentMissionSend, prepare, seedAndOpen, sendCount } from "./helpers";

test.beforeEach(async ({ page }) => {
  await prepare(page);
});

test("U04: 조합 중 Enter는 전송하지 않고, 조합 확정 뒤 Enter만 전송한다", async ({ page }) => {
  await seedAndOpen(page, { agents: 1 });
  await instrumentMissionSend(page);

  await page.getByTestId("toggle-lead").click();
  const composer = page.getByTestId("lead-composer");
  await composer.click();

  // 실제 IME 조합 시작(Chromium CDP).
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Input.imeSetComposition", { text: "안녕하세요", selectionStart: 5, selectionEnd: 5 });

  // 조합 확정 Enter — 전송 금지(05 §4).
  await page.keyboard.press("Enter");
  expect(await sendCount(page)).toBe(0);

  // 조합 종료 후 draft를 확정된 텍스트로 두고 Enter → 1회 전송.
  await cdp.send("Input.imeSetComposition", { text: "", selectionStart: 0, selectionEnd: 0 });
  await page.evaluate(() => {
    const el = document.querySelector("[data-testid='lead-composer']") as HTMLTextAreaElement | null;
    el?.dispatchEvent(new CompositionEvent("compositionend", { bubbles: true }));
  });
  await composer.fill("안녕하세요");
  await page.keyboard.press("Enter");
  await expect.poll(() => sendCount(page), { timeout: 10_000 }).toBe(1);
  // 전송 성공 후 draft가 비워진다.
  await expect(composer).toHaveValue("");
});
