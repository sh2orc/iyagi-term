import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { prepare, seedAndOpen, harnessAddMessage } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  test(`전달 불명 메시지를 확인하고 새 지시로 연결한다: ${layout}`, async ({ page }, testInfo) => {
    await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
    await prepare(page);
    const missionId = await seedAndOpen(page, { agents: 1 });
    const sourceId = await harnessAddMessage(page, missionId, { role: "user", delivery: "unknown", text: "외부 요청 전에 입력값을 확인해 주세요." });
    if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
    const source = page.locator(`[id="mission-message-${sourceId}"]`);
    await source.getByTestId("message-replace-open").click();
    await expect(source).toContainText("이전 지시가 이미 반영되었을 수 있습니다");
    const input = source.getByTestId("message-replacement-body");
    await expect(input).toHaveValue("외부 요청 전에 입력값을 확인해 주세요.");
    await input.fill("기록을 확인했습니다. 이미 완료한 외부 요청은 반복하지 말고 입력 검증을 보완해 주세요.");
    const send = source.getByTestId("message-replacement-send");
    await send.scrollIntoViewIfNeeded();
    await expect(send).toBeInViewport({ ratio: 0.99 });
    const width = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
    expect(width.scroll).toBeLessThanOrEqual(width.width + 1);
    await page.screenshot({ path: testInfo.outputPath(`message-replacement-${layout}.png`) });
    await send.click();
    await expect(source.getByTestId("message-replacement-link")).toBeVisible();
    await expect(source).toContainText("전달 여부 확인 필요");
    const result = await page.evaluate(async ({ missionId, sourceId }) => {
      const client = window.__mockDaemon!.client as MockDaemonClient;
      const page = await client.missionSnapshot({ mission_id: missionId, snapshot_id: null, cursor: null });
      const messages = page.entities.flatMap(e => "Message" in e ? [e.Message] : []);
      const replacements = messages.filter(m => m.supersedes_message_id === sourceId);
      const body = await client.artifactRead({ artifact_id: replacements[0].body_ref.id, offset: "0", max_bytes: 4096 });
      const text = new TextDecoder().decode(Uint8Array.from(atob(body.data_b64), c => c.charCodeAt(0)));
      return { count: replacements.length, nextId: replacements[0].id, delivery: replacements[0].delivery, text };
    }, { missionId, sourceId });
    expect(result.count).toBe(1);
    expect(result.delivery).toBe("queued");
    expect(result.text).toContain("이미 완료한 외부 요청은 반복하지 말고");
    const next = page.locator(`[id="mission-message-${result.nextId}"]`);
    await source.getByTestId("message-replacement-link").click();
    await expect(next).toBeFocused();
    await next.getByTestId("message-original-link").click();
    await expect(source).toBeFocused();
    await expect(source.getByTestId("message-replace-open")).toHaveCount(0);
  });
}
