/**
 * test:mission-ui 공용 도우미(O16). plain-browser 미리보기
 * (MockDaemonClient + window.__mockDaemon harness)를 잡고 시나리오를 심는다.
 * evaluate 콜백은 브라우저 안에서 실행되므로 Node 쪽 클로저를 닫지 않는다 —
 * 모든 콜백은 window.__mockDaemon에 직접 접근하는 자기완결 함수다.
 */

import { expect, type Page } from "@playwright/test";
import type { DaemonClient } from "../../src/features/daemon/client";

/** 첫 실행 셸 선택 모달이 뜨지 않게 기본 프로필 + 한국어를 미리 심는다. */
export async function prepare(page: Page): Promise<void> {
  await page.addInitScript(() => {
    window.localStorage.setItem(
      "iyagi.shellprofiles.v1",
      JSON.stringify({ state: { custom: [], defaultProfileId: "powershell" }, version: 0 }),
    );
    window.localStorage.setItem(
      "iyagi.language.v1",
      JSON.stringify({ state: { language: "ko" }, version: 0 }),
    );
  });
  await page.goto("/");
  await expect(page.getByTestId("new-tab-button")).toBeVisible({ timeout: 20_000 });
}

interface HarnessApi {
  reset(): void;
  seedMission(options?: { title?: string; goal?: string; agents?: number }): Promise<string>;
  addAgents(missionId: string, count: number): Promise<string[]>;
  addMessage(
    missionId: string,
    options: { role?: string; text: string; delivery?: string },
  ): Promise<string>;
  openDecision(missionId: string, options?: { blocking?: boolean }): Promise<string>;
  stageForAcceptance(missionId: string): Promise<{ candidateId: string }>;
  replaceCandidate(missionId: string): Promise<{ candidateId: string }>;
  failNextStart(): void;
  missions(): Array<{ id: string; state: string; title: string }>;
  openMissionTab(missionId: string): string | null;
  getActiveTabId(): string | null;
  getTabCount(): number;
  readonly client: Pick<DaemonClient, "missionMessage">;
}

type HarnessWindow = Window & { __mockDaemon?: HarnessApi };

/** mission을 심어 탭으로 연다. 반환은 mission id. */
export async function seedAndOpen(
  page: Page,
  options: { title?: string; goal?: string; agents?: number } = {},
): Promise<string> {
  const missionId = await page.evaluate((opts) => {
    const w = window as unknown as HarnessWindow;
    const api = w.__mockDaemon;
    if (!api) throw new Error("window.__mockDaemon is not installed");
    api.reset();
    return api.seedMission(opts);
  }, options);
  await page.evaluate((id) => {
    const w = window as unknown as HarnessWindow;
    const api = w.__mockDaemon;
    if (!api) throw new Error("window.__mockDaemon is not installed");
    api.openMissionTab(id);
  }, missionId);
  await expect(page.getByTestId("mission-page")).toBeVisible();
  return missionId;
}

export function harnessAddAgents(page: Page, missionId: string, count: number): Promise<string[]> {
  return page.evaluate(
    ([id, n]) => {
      const api = (window as unknown as HarnessWindow).__mockDaemon;
      if (!api) throw new Error("window.__mockDaemon is not installed");
      return api.addAgents(id, n);
    },
    [missionId, count] as [string, number],
  );
}

export function harnessAddMessage(
  page: Page,
  missionId: string,
  options: { role?: string; text: string; delivery?: string },
): Promise<string> {
  return page.evaluate(
    ([id, opts]) => {
      const api = (window as unknown as HarnessWindow).__mockDaemon;
      if (!api) throw new Error("window.__mockDaemon is not installed");
      return api.addMessage(id, opts);
    },
    [missionId, options] as [string, { role?: string; text: string; delivery?: string }],
  );
}

export function harnessOpenDecision(
  page: Page,
  missionId: string,
  options: { blocking?: boolean } = {},
): Promise<string> {
  return page.evaluate(
    ([id, opts]) => {
      const api = (window as unknown as HarnessWindow).__mockDaemon;
      if (!api) throw new Error("window.__mockDaemon is not installed");
      return api.openDecision(id, opts);
    },
    [missionId, options] as [string, { blocking?: boolean }],
  );
}

export function harnessStageForAcceptance(page: Page, missionId: string): Promise<{ candidateId: string }> {
  return page.evaluate((id) => {
    const api = (window as unknown as HarnessWindow).__mockDaemon;
    if (!api) throw new Error("window.__mockDaemon is not installed");
    return api.stageForAcceptance(id);
  }, missionId);
}

export function harnessReplaceCandidate(page: Page, missionId: string): Promise<{ candidateId: string }> {
  return page.evaluate((id) => {
    const api = (window as unknown as HarnessWindow).__mockDaemon;
    if (!api) throw new Error("window.__mockDaemon is not installed");
    return api.replaceCandidate(id);
  }, missionId);
}

export function harnessFailNextStart(page: Page): Promise<void> {
  return page.evaluate(() => {
    const api = (window as unknown as HarnessWindow).__mockDaemon;
    if (!api) throw new Error("window.__mockDaemon is not installed");
    api.failNextStart();
  });
}

export function harnessMissions(
  page: Page,
): Promise<Array<{ id: string; state: string; title: string }>> {
  return page.evaluate(() => {
    const api = (window as unknown as HarnessWindow).__mockDaemon;
    if (!api) throw new Error("window.__mockDaemon is not installed");
    return api.missions();
  });
}

export function harnessGetActiveTabId(page: Page): Promise<string | null> {
  return page.evaluate(() => {
    const api = (window as unknown as HarnessWindow).__mockDaemon;
    if (!api) throw new Error("window.__mockDaemon is not installed");
    return api.getActiveTabId();
  });
}

export function harnessGetTabCount(page: Page): Promise<number> {
  return page.evaluate(() => {
    const api = (window as unknown as HarnessWindow).__mockDaemon;
    if (!api) throw new Error("window.__mockDaemon is not installed");
    return api.getTabCount();
  });
}

/** mission.message 호출 수를 세는 창구(U04 전송 횟수 단언). */
export async function instrumentMissionSend(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as HarnessWindow & { __missionSendCount?: () => number };
    const mockApi = w.__mockDaemon;
    if (!mockApi) throw new Error("window.__mockDaemon is not installed");
    let count = 0;
    const original = mockApi.client.missionMessage.bind(mockApi.client);
    mockApi.client.missionMessage = async params => {
      count += 1;
      return original(params);
    };
    w.__missionSendCount = () => count;
  });
}

export function sendCount(page: Page): Promise<number> {
  return page.evaluate(
    () => (window as unknown as { __missionSendCount?: () => number }).__missionSendCount?.() ?? 0,
  );
}
