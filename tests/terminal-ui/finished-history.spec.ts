import { expect, test } from "@playwright/test";

const SEPARATOR = /Previous output · continued in a new shell|이전 출력 · 새 셸에서 이어서/;

test("finished terminal keeps history visible and scrollable, and connecting it continues below that history", async ({ page }) => {
  await page.goto("/", { waitUntil: "load" });
  await page.evaluate(async () => {
    const load = (path: string) => import(path);
    const { usePreferences } = await load("/src/store/preferences.ts");
    usePreferences.getState().setGpuRenderer(false);
  });
  await page.getByRole("dialog").getByRole("button", { name: /^zsh/ }).click();
  const pane = page.locator('.pane-body[data-phase="live"]').first();
  await expect(pane).toBeVisible();
  await page.evaluate(async () => {
    const load = (path: string) => import(path);
    const { useWorkbenchStore } = await load("/src/store/workbenchStore.ts");
    const state = useWorkbenchStore.getState();
    const client = (window as unknown as { __mockDaemon: { client: {
      emitProgramOutput(id: string, bytes: Uint8Array): void;
      killSession(id: string, exitCode?: number): void;
    } } }).__mockDaemon.client;
    const id = state.panes[state.focusedLeafId].sessionId;
    client.emitProgramOutput(id, new TextEncoder().encode(Array.from({ length: 80 }, (_, i) => `history line ${i}\r\n`).join("")));
    // 정상 종료(코드 0)는 창을 바로 닫는다(04-ui §5-1) — 끝난 창의 화면을 보는 검사라 비정상 종료로 세운다.
    client.killSession(id, 1);
  });

  // The pane that just finished: its history stays above the exit actions and scrolls.
  const finished = page.locator('.pane-body[data-phase="exited"]').first();
  await expect(finished.locator(".pane-overlay-exited")).toBeVisible();
  await expect.poll(async () => {
    const screen = await finished.locator(".terminal-view").boundingBox();
    const actions = await finished.locator(".pane-overlay-exited").boundingBox();
    return Boolean(screen && actions && screen.height > 150 && screen.y + screen.height <= actions.y + 1);
  }).toBe(true);
  await page.screenshot({ path: "node_modules/.cache/terminal-ui/finished-history.png" });
  await finished.locator(".xterm-screen").hover();
  // xterm normalizes each wheel event to a few lines on macOS.
  for (let i = 0; i < 20; i++) await page.mouse.wheel(0, -600);
  await expect(finished.locator(".xterm-rows")).toContainText("history line 0");

  // Recently finished → connect: the journal replays, then a fresh shell continues below it.
  await page.locator(".pane-close").first().click();
  await page.getByRole("button", { name: "Open workload queue" }).click();
  await page.locator(".finished-row button").first().click();
  const continued = page.locator('.pane-body[data-phase="live"]').first();
  await expect(continued).toBeVisible();
  await expect(continued.locator(".xterm-rows")).toContainText(SEPARATOR);
  // The continued terminal is a running workload again, so the row leaves the finished list.
  await expect(page.locator(".finished-row")).toHaveCount(0);
  await continued.locator(".xterm-screen").hover();
  for (let i = 0; i < 20; i++) await page.mouse.wheel(0, -600);
  await expect(continued.locator(".xterm-rows")).toContainText("history line 0");
  await page.screenshot({ path: "node_modules/.cache/terminal-ui/finished-history-continued.png" });
});
