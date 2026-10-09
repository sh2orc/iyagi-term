import { chromium, webkit, expect, test, type Page } from "@playwright/test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Mission } from "../../src/generated/Mission";
import type { Entity } from "../../src/generated/Entity";
import type { ArtifactRef } from "../../src/generated/ArtifactRef";
import type { MissionMessageParams } from "../../src/generated/MissionMessageParams";
import type { MockDaemonClient } from "../../src/features/daemon/mockClient";
import { harnessAddMessage, prepare, seedAndOpen } from "./helpers";

// The browser process is restarted while the fixture daemon's immutable data
// remains in the test driver. Product recovery metadata is real IndexedDB data.
type FixtureStore = {
  missionStates: Map<string, { mission: Mission; entities: Map<string, Entity>; events: unknown[] }>;
  artifacts: Map<string, { ref: ArtifactRef; bytes: Uint8Array }>;
  requests: Map<string, unknown>;
};
type ProbeWindow = Window & { __requestProbe?: () => { calls: MissionMessageParams[]; uploads: number } };

async function captureFixture(page: Page) {
  return page.evaluate(() => {
    const store = window.__mockDaemon!.client as unknown as FixtureStore;
    return {
      missions: [...store.missionStates].map(([id, value]) => [id, { ...value, entities: [...value.entities] }] as const),
      artifacts: [...store.artifacts].map(([id, value]) => [id, { ...value, bytes: [...value.bytes] }] as const),
      requests: [...store.requests],
    };
  });
}

async function instrument(page: Page, loseReceipt: boolean) {
  await page.evaluate(lose => {
    const client = window.__mockDaemon!.client as MockDaemonClient;
    const actual = client.missionMessage.bind(client), begin = client.artifactBegin.bind(client);
    const calls: MissionMessageParams[] = []; let uploads = 0;
    client.missionMessage = async params => {
      calls.push(structuredClone(params));
      const result = await actual(params);
      if (lose) throw new Error("저장 응답 연결이 끊어졌습니다.");
      return result;
    };
    client.artifactBegin = params => { uploads += 1; return begin(params); };
    (window as ProbeWindow).__requestProbe = () => ({ calls, uploads });
  }, loseReceipt);
}

async function storedRequest(page: Page, params: MissionMessageParams) {
  return page.evaluate(async p => {
    const path = "/src/features/missions/messageJournal.ts";
    const journal = await import(/* @vite-ignore */ path) as typeof import("../../src/features/missions/messageJournal");
    return journal.getMessageJournal().load(journal.messageScope(p.mission_id, p.target_task_id, p.supersedes_message_id ?? null));
  }, params);
}

for (const browserName of ["chromium", "webkit"] as const) {
test.describe(browserName, () => {
  const browser = browserName === "webkit" ? webkit : chromium;
for (const layout of ["700", "zoom200"] as const) {
  for (const recipient of ["lead", "task", "replacement"] as const) {
    test(`브라우저 종료·재시작 뒤 ${recipient} 요청을 복원한다: ${layout}`, async ({}, testInfo) => {
      const profile = await mkdtemp(join(tmpdir(), "iyagi-message-restart-"));
      const options = { headless: true, baseURL: "http://localhost:5183", viewport: { width: layout === "700" ? 700 : 1440, height: 900 } };
      let context = await browser.launchPersistentContext(profile, options);
      try {
        let page = await context.newPage(); await prepare(page);
        const missionId = await seedAndOpen(page, { agents: 2 });
        const sourceId = recipient === "replacement" ? await harnessAddMessage(page, missionId, { role: "user", delivery: "unknown", text: "元の指示: 외부 요청 기록 확인" }) : null;
        const selectComposer = async (fresh: boolean) => {
          if (layout === "zoom200") await page.evaluate(() => { document.body.style.zoom = "2"; });
          if (recipient === "task") {
            await page.getByTestId("toggle-detail").click();
            await page.getByTestId("team-row").first().click();
            await page.getByTestId("instruct-toggle").click();
          }
          if (recipient !== "task") await page.getByTestId("toggle-lead").click();
          if (recipient === "replacement") {
            const source = page.locator(`[id="mission-message-${sourceId}"]`);
            if (fresh) await source.getByTestId("message-replace-open").click();
            return { input: source.getByTestId("message-replacement-body"), send: source.getByTestId("message-replacement-send") };
          }
          return { input: page.getByTestId(recipient === "lead" ? "lead-composer" : "task-composer-input"),
            send: page.getByTestId(recipient === "lead" ? "lead-send" : "task-composer-send") };
        };
        await instrument(page, true);
        let composer = await selectComposer(true);
        const text = "再開 · 재시작 뒤에도 동일한 외부 요청을 반복하지 마세요. 😀";
        await composer.input.fill(text); await composer.send.click();
        await expect(composer.send).toHaveText("전송 결과 다시 확인");
        await expect(composer.send).toBeEnabled();
        const first = await page.evaluate(() => (window as ProbeWindow).__requestProbe!());
        expect(first.calls).toHaveLength(1);
        const request = first.calls[0];
        expect(await storedRequest(page, request)).toEqual(request);
        expect(JSON.stringify(await storedRequest(page, request))).not.toContain(text);
        const fixture = await captureFixture(page);
        // Close the browser process and reopen the same persistent profile.
        await context.close();
        context = await browser.launchPersistentContext(profile, options);
        page = await context.newPage(); await prepare(page);
        await page.evaluate(data => {
          const store = window.__mockDaemon!.client as unknown as FixtureStore;
          store.missionStates.clear(); store.artifacts.clear(); store.requests.clear();
          for (const [id, value] of data.missions) store.missionStates.set(id, { ...value, entities: new Map(value.entities) });
          for (const [id, value] of data.artifacts) store.artifacts.set(id, { ...value, bytes: new Uint8Array(value.bytes) });
          for (const [id, value] of data.requests) store.requests.set(id, value);
        }, fixture);
        await instrument(page, false);
        await page.evaluate(id => window.__mockDaemon!.openMissionTab(id), missionId);
        await expect(page.getByTestId("mission-page")).toBeVisible();
        composer = await selectComposer(false);
        await expect(composer.input).toHaveValue(text);
        await expect(composer.input).toBeDisabled();
        await expect(composer.send).toBeEnabled();
        expect((await page.evaluate(() => (window as ProbeWindow).__requestProbe!())).calls).toHaveLength(0);
        await composer.send.scrollIntoViewIfNeeded();
        await expect(composer.send).toBeInViewport({ ratio: 0.99 });
        if (recipient !== "replacement") {
          const note = page.getByTestId("message-receipt-note");
          await note.scrollIntoViewIfNeeded();
          await expect(note).toBeInViewport({ ratio: 0.99 });
          await expect(composer.send).toBeInViewport({ ratio: 0.99 });
        }
        const width = await page.evaluate(() => ({ viewport: document.documentElement.clientWidth, scroll: document.documentElement.scrollWidth }));
        expect(width.scroll).toBeLessThanOrEqual(width.viewport + 1);
        await page.screenshot({ path: testInfo.outputPath(`restored-${recipient}-${layout}.png`) });
        await composer.send.click();
        await expect.poll(async () => storedRequest(page, request)).toBeNull();
        const probe = await page.evaluate(() => (window as ProbeWindow).__requestProbe!());
        expect(probe.calls).toEqual([request]); expect(probe.uploads).toBe(0);
        const after = await captureFixture(page);
        expect(after.missions).toEqual(fixture.missions);
        expect(after.requests).toEqual(fixture.requests);
        expect(after.artifacts).toEqual(fixture.artifacts);
      } finally {
        await context.close();
        await rm(profile, { recursive: true, force: true });
      }
    });
  }
}

test("서로 다른 창의 IndexedDB transaction은 같은 수신자의 요청 하나만 저장한다", async () => {
  const browserProcess = await browser.launch({ headless: true });
  const context = await browserProcess.newContext({ baseURL: "http://localhost:5183" });
  try {
  const page = await context.newPage();
  await prepare(page);
  const other = await context.newPage(); await prepare(other);
  const id = crypto.randomUUID();
  const params: MissionMessageParams = { request_id: crypto.randomUUID(), mission_id: id, expected_revision: "3", target_task_id: null,
    body_ref: { id: crypto.randomUUID(), sha256: "0".repeat(64), bytes: "10", media_type: "text/plain" } };
  const second = { ...params, request_id: crypto.randomUUID() };
  const save = (window: Page, p: MissionMessageParams) => window.evaluate(async request => {
    const path = "/src/features/missions/messageJournal.ts";
    const api = await import(/* @vite-ignore */ path) as typeof import("../../src/features/missions/messageJournal");
    try { await api.createMessageJournal().save(request); return "saved"; }
    catch (error) { if (error instanceof api.PendingMessageConflict) return "conflict"; throw error; }
  }, p);
  const results = await Promise.all([save(page, params), save(other, second)]);
  expect(results.sort()).toEqual(["conflict", "saved"]);
  const stored = await storedRequest(page, params);
  expect([params.request_id, second.request_id]).toContain(stored!.request_id);
  const loser = stored!.request_id === params.request_id ? second : params;
  const clearResult = await other.evaluate(async request => {
    const path = "/src/features/missions/messageJournal.ts";
    const api = await import(/* @vite-ignore */ path) as typeof import("../../src/features/missions/messageJournal");
    try { await api.createMessageJournal().forget(request); return "cleared"; }
    catch (error) { if (error instanceof api.PendingMessageConflict) return "conflict"; throw error; }
  }, loser);
  expect(clearResult).toBe("conflict");
  expect(await storedRequest(other, params)).toEqual(stored);
  await other.close();
  } finally { await browserProcess.close(); }
});

});
}
