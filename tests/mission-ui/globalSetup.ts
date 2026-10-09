/**
 * test:mission-ui globalSetup — 콜드 vite 변환(대형 모듈 그래프)을 시험
 * timeout 바깥에서 한 번만 데운다. 느린 디스크/백신 환경에서 첫 탐색이
 * 분 단위로 걸리는 것을 흡수한다(이후 시험은 warm server를 본다).
 */

import { chromium, type FullConfig } from "@playwright/test";

export default async function warmUp(config: FullConfig): Promise<() => Promise<void>> {
  const baseURL = config.projects[0]?.use?.baseURL ?? "http://localhost:5183";
  const browser = await chromium.launch();
  const page = await browser.newPage();
  // load 이벤트가 모든 모듈 변환을 기다린다 — 시험 timeout과 무관하게 넉넉히.
  await page.goto(baseURL, { waitUntil: "load", timeout: 300_000 });
  await page.waitForSelector("[data-testid='new-tab-button']", { timeout: 120_000 });
  await page.close();
  await browser.close();
  return async () => undefined;
}
