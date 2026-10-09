import { expect, test } from "@playwright/test";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import type { MissionMessageParams } from "../../src/generated/MissionMessageParams";
import { prepare, seedAndOpen } from "./helpers";

for (const layout of ["700", "zoom200"] as const) {
  for (const recipient of ["lead", "task"] as const) {
    test(`전송 응답 손실 후 같은 ${recipient} 요청을 확인한다: ${layout}`, async ({ page }, testInfo) => {
      await page.setViewportSize({ width: layout === "700" ? 700 : 1440, height: 900 });
      await prepare(page);
      const missionId = await seedAndOpen(page, { agents: 2 });
      if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
      const before = await page.evaluate(async id => {
        const client = window.__mockDaemon!.client as MockDaemonClient;
        const original = client.missionMessage.bind(client);
        const calls: MissionMessageParams[] = [];
        client.missionMessage = async params => {
          calls.push(structuredClone(params));
          const result = await original(params);
          if (calls.length === 1) throw new Error("저장 응답 연결이 끊어졌습니다.");
          if (JSON.stringify(calls[0]) !== JSON.stringify(params)) throw new Error("Retry changed the original request");
          return result;
        };
        const snapshot = await client.missionSnapshot({ mission_id: id, snapshot_id: null, cursor: null });
        return snapshot.entities.flatMap(e => "Message" in e && e.Message.role === "user" ? [e.Message] : []);
      }, missionId);
      if (recipient === "task") {
        await page.getByTestId("toggle-detail").click();
        await page.getByTestId("team-row").first().click();
        await page.getByTestId("instruct-toggle").click();
      }
      if (recipient === "lead") await page.getByTestId("toggle-lead").click();
      const input = page.getByTestId(recipient === "lead" ? "lead-composer" : "task-composer-input");
      const send = page.getByTestId(recipient === "lead" ? "lead-send" : "task-composer-send");
      const body = "외부 요청 기록을 먼저 확인하고 입력 검증을 보완해 주세요.";
      await input.fill(body);
      await send.click();
      await expect(send).toHaveText("전송 결과 다시 확인");
      await expect(input).toBeDisabled();
      await expect(page.getByTestId("message-receipt-note")).toBeVisible();
      if (recipient === "lead") {
        await page.getByTestId("toggle-detail").click();
        await expect(input).toHaveCount(0);
        await page.getByTestId("toggle-lead").click();
      } else {
        await page.getByTestId("instruct-toggle").click();
        await expect(input).toHaveCount(0);
        await page.getByTestId("instruct-toggle").click();
      }
      await expect(input).toHaveValue(body);
      await expect(input).toBeDisabled();
      await expect(send).toBeEnabled();
      await send.scrollIntoViewIfNeeded();
      await expect(send).toBeInViewport({ ratio: 0.99 });
      const note = page.getByTestId("message-receipt-note");
      await note.scrollIntoViewIfNeeded();
      await expect(note).toBeInViewport({ ratio: 0.99 });
      await expect(send).toBeInViewport({ ratio: 0.99 });
      await expect(page.getByRole("alert")).toContainText("저장 응답 확인 필요");
      const size = await page.evaluate(() => ({ width: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
      expect(size.scroll).toBeLessThanOrEqual(size.width + 1);
      await page.screenshot({ path: testInfo.outputPath(`message-receipt-${recipient}-${layout}.png`) });
      await send.click();
      await expect(input).toHaveValue("");
      await expect(page.getByTestId("message-receipt-note")).toHaveCount(0);
      const messages = await page.evaluate(async id => {
        const client = window.__mockDaemon!.client as MockDaemonClient;
        const snapshot = await client.missionSnapshot({ mission_id: id, snapshot_id: null, cursor: null });
        return snapshot.entities.flatMap(e => "Message" in e && e.Message.role === "user" ? [e.Message] : []);
      }, missionId);
      const added = messages.filter(message => !before.some(old => old.id === message.id));
      expect(messages).toHaveLength(before.length + 1);
      expect(messages.filter(message => before.some(old => old.id === message.id))).toEqual(before);
      expect(added).toHaveLength(1);
      expect(added[0].target_task_id === null).toBe(recipient === "lead");
    });
  }
}
