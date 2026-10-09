/**
 * 데몬 오래됨 배너(리뷰 Finding 1)의 상호작용 — happy-dom(*.dom.test.tsx).
 *
 * transportStatus().daemonOutdated 가 참일 때만 뜨고, "재시작"을 누르면
 * client.restartDaemon()을 부른 뒤 다시 polling 해서 최신이면 사라진다.
 * transportStatus/restartDaemon seam이 없는(브라우저 mock) 클라이언트에서는
 * 아예 그리지 않는다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { t, useI18nStore } from "../../i18n";
import type { DaemonClient } from "../daemon/client";
import { DaemonOutdatedBanner } from "./DaemonOutdatedBanner";

let root: Root | null = null;
let container: HTMLElement | null = null;

/** 배너가 쓰는 seam만 갖춘 최소 DaemonClient. */
function fakeClient(
  overrides: Partial<{
    transportStatus: DaemonClient["transportStatus"];
    restartDaemon: DaemonClient["restartDaemon"];
  }> = {},
): DaemonClient {
  return {
    events: { subscribe: () => ({ dispose: () => {} }) },
    ...overrides,
  } as unknown as DaemonClient;
}

async function renderBanner(client: DaemonClient): Promise<HTMLElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(<DaemonOutdatedBanner client={client} />);
  });
  // 최초 effect가 던진 비동기 refresh(상태 갱신)를 흘려보낸다.
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
  return container as HTMLElement;
}

beforeEach(() => {
  useI18nStore.setState({ language: "en" });
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
  vi.restoreAllMocks();
});

describe("DaemonOutdatedBanner", () => {
  it("데몬이 오래됐으면 배너를 그리고 재시작 버튼이 shutdown 경로를 부른다", async () => {
    let outdated = true;
    const transportStatus = vi.fn(async () => ({
      controlAlive: true,
      dataAlive: true,
      generation: 0,
      daemonOutdated: outdated,
    }));
    // 재시작하면 최신 데몬으로 갈아탄 것으로 흉내 낸다(이후 polling은 최신).
    const restartDaemon = vi.fn(async () => {
      outdated = false;
    });
    const client = fakeClient({ transportStatus, restartDaemon });

    const el = await renderBanner(client);
    expect(el.querySelector("[data-testid='daemon-outdated-banner']")).not.toBeNull();
    expect(el.textContent).toContain(t("daemon.outdated.message"));

    const button = el.querySelector<HTMLButtonElement>("[data-testid='daemon-outdated-restart']");
    expect(button?.textContent).toBe(t("daemon.outdated.restart"));

    await act(async () => {
      button?.click();
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(restartDaemon).toHaveBeenCalledTimes(1);
    // 최신으로 바뀐 뒤 재-polling → 배너가 사라진다.
    expect(container?.querySelector("[data-testid='daemon-outdated-banner']")).toBeNull();
  });

  it("데몬이 최신이면 아무것도 그리지 않는다", async () => {
    const transportStatus = vi.fn(async () => ({
      controlAlive: true,
      dataAlive: true,
      generation: 0,
      daemonOutdated: false,
    }));
    const client = fakeClient({ transportStatus, restartDaemon: vi.fn() });
    const el = await renderBanner(client);
    expect(el.querySelector("[data-testid='daemon-outdated-banner']")).toBeNull();
  });

  it("transport seam이 없는 클라이언트(브라우저 mock)에서는 뜨지 않는다", async () => {
    const el = await renderBanner(fakeClient());
    expect(el.querySelector("[data-testid='daemon-outdated-banner']")).toBeNull();
  });

  it("재시작이 실패하면 배너를 유지하고 수동 재시작 안내를 보여 준다", async () => {
    const transportStatus = vi.fn(async () => ({
      controlAlive: true,
      dataAlive: true,
      generation: 0,
      daemonOutdated: true,
    }));
    const restartDaemon = vi.fn(async () => {
      throw new Error("spawn failed");
    });
    const client = fakeClient({ transportStatus, restartDaemon });

    const el = await renderBanner(client);
    const button = el.querySelector<HTMLButtonElement>("[data-testid='daemon-outdated-restart']");
    await act(async () => {
      button?.click();
    });
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(restartDaemon).toHaveBeenCalledTimes(1);
    expect(container?.querySelector("[data-testid='daemon-outdated-banner']")).not.toBeNull();
    expect(container?.textContent).toContain(t("daemon.outdated.restartFailed"));
  });
});
