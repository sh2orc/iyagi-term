/**
 * 설정 → 일반의 데몬 재시작 행 — happy-dom(*.dom.test.tsx).
 *
 * 상태 줄은 transportStatus().daemonOutdated를 보여 주고, "재시작"은 확인을
 * 거친 뒤에만 restartDaemon({ force: true })를 부른다. 재시작 seam이 없는
 * (브라우저 mock) 클라이언트에서는 행을 아예 그리지 않는다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { t, useI18nStore } from "../../i18n";
import type { DaemonClient } from "../daemon/client";
import { GeneralPanel } from "./GeneralPanel";

let root: Root | null = null;
let container: HTMLElement | null = null;

/** 행이 쓰는 seam만 갖춘 최소 DaemonClient. */
function fakeClient(
  overrides: Partial<{
    transportStatus: DaemonClient["transportStatus"];
    restartDaemon: DaemonClient["restartDaemon"];
  }> = {},
): DaemonClient {
  return { ...overrides } as unknown as DaemonClient;
}

async function flush(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function render(client: DaemonClient): Promise<HTMLElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root?.render(<GeneralPanel client={client} />);
  });
  await flush();
  return container as HTMLElement;
}

async function click(el: Element | null): Promise<void> {
  expect(el).not.toBeNull();
  await act(async () => {
    (el as HTMLButtonElement).click();
  });
  await flush();
}

const status = (el: HTMLElement) => el.querySelector("[data-testid='daemon-restart-status']")?.textContent;

beforeEach(() => {
  useI18nStore.setState({ language: "ko" });
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
  vi.restoreAllMocks();
});

describe("DaemonRestartRow", () => {
  it("오래된 데몬을 알리고, 확인을 거쳐 강제 재시작한다", async () => {
    let outdated = true;
    const transportStatus = vi.fn(async () => ({
      controlAlive: true,
      dataAlive: true,
      generation: 0,
      daemonOutdated: outdated,
    }));
    const restartDaemon = vi.fn(async () => {
      outdated = false;
    });
    const el = await render(fakeClient({ transportStatus, restartDaemon }));
    expect(el.querySelector("[data-setting-id='daemonRestart']")).not.toBeNull();
    expect(status(el)).toBe(t("settings.item.daemonRestart.outdated"));

    // 첫 클릭은 확인만 띄운다 — 취소하면 아무것도 하지 않는다.
    await click(el.querySelector("[data-testid='daemon-restart']"));
    expect(el.textContent).toContain(t("settings.item.daemonRestart.confirm"));
    const cancel = [...el.querySelectorAll("button")].find((b) => b.textContent === t("settings.cancel"));
    await click(cancel ?? null);
    expect(restartDaemon).not.toHaveBeenCalled();
    expect(el.textContent).not.toContain(t("settings.item.daemonRestart.confirm"));

    await click(el.querySelector("[data-testid='daemon-restart']"));
    await click(el.querySelector("[data-testid='daemon-restart-confirm']"));
    expect(restartDaemon).toHaveBeenCalledWith({ force: true });
    expect(status(el)).toBe(t("settings.item.daemonRestart.done"));
    // 재시작 뒤 상태를 다시 읽는다.
    expect(transportStatus).toHaveBeenCalledTimes(2);
  });

  it("최신 데몬도 재시작할 수 있고, 실패하면 오류를 보인다", async () => {
    const transportStatus = vi.fn(async () => ({
      controlAlive: true,
      dataAlive: true,
      generation: 0,
      daemonOutdated: false,
    }));
    const restartDaemon = vi.fn(async () => {
      throw { code: "DAEMON_UNAVAILABLE", message: "timed out" };
    });
    const el = await render(fakeClient({ transportStatus, restartDaemon }));
    expect(status(el)).toBe(t("settings.item.daemonRestart.current"));

    await click(el.querySelector("[data-testid='daemon-restart']"));
    await click(el.querySelector("[data-testid='daemon-restart-confirm']"));
    expect(restartDaemon).toHaveBeenCalledWith({ force: true });
    expect(el.textContent).toContain(t("settings.item.daemonRestart.failed"));
    const button = el.querySelector<HTMLButtonElement>("[data-testid='daemon-restart']");
    expect(button?.disabled).toBe(false);
  });

  it("재시작 seam이 없는 클라이언트에서는 행을 그리지 않는다", async () => {
    const el = await render(fakeClient());
    expect(el.querySelector("[data-setting-id='daemonRestart']")).toBeNull();
    expect(el.querySelector("[data-setting-id='theme']")).not.toBeNull();
  });
});
