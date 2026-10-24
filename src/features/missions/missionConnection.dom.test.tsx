/**
 * mission 화면의 연결 상태(05 §10)는 따로 폴링하지 않고 세션 제어기의 전송 점검
 * 결과(daemon/transportFeed)를 받는다. 처음 붙을 때 아직 관찰이 없으면 한 번만
 * 직접 읽는다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import type { DaemonClient } from "../daemon/client";
import { publishTransportStatus, resetTransportFeedForTests } from "../daemon/transportFeed";
import { resetMissionClientForTests, setMissionClient } from "./clientAccess";
import { resetMissionConnectionForTests, useMissionConnection, useMissionConnectionStore } from "./missionConnection";

let root: Root | null = null;
let container: HTMLElement | null = null;

function Probe(): null {
  useMissionConnection();
  return null;
}

function mount(): void {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => root?.render(<Probe />));
}

afterEach(() => {
  act(() => root?.unmount());
  root = null;
  container?.remove();
  container = null;
  vi.useRealTimers();
  resetTransportFeedForTests();
  resetMissionClientForTests();
  resetMissionConnectionForTests();
});

describe("useMissionConnection", () => {
  it("제어기의 관찰을 받아 반영하고 따로 타이머를 돌리지 않는다", async () => {
    vi.useFakeTimers();
    const transportStatus = vi.fn(async () => ({ controlAlive: true, dataAlive: true, generation: 1, daemonOutdated: false }));
    setMissionClient({ transportStatus } as unknown as DaemonClient);
    publishTransportStatus({ connected: true, generation: 1 });
    mount();
    expect(useMissionConnectionStore.getState().status).toBe("connected");
    expect(transportStatus).not.toHaveBeenCalled();
    expect(vi.getTimerCount()).toBe(0);

    act(() => publishTransportStatus({ connected: false, generation: null }));
    expect(useMissionConnectionStore.getState().status).toBe("disconnected");
    act(() => publishTransportStatus({ connected: true, generation: 2 }));
    expect(useMissionConnectionStore.getState()).toMatchObject({ status: "connected", generation: 2, reconnects: 1 });
    await vi.advanceTimersByTimeAsync(30_000);
    expect(transportStatus).not.toHaveBeenCalled();
  });

  it("아직 관찰이 없으면 처음 한 번만 직접 읽는다", async () => {
    const transportStatus = vi.fn(async () => ({ controlAlive: true, dataAlive: false, generation: 3, daemonOutdated: false }));
    setMissionClient({ transportStatus } as unknown as DaemonClient);
    mount();
    await act(async () => {
      for (let i = 0; i < 4; i++) await Promise.resolve();
    });
    expect(transportStatus).toHaveBeenCalledTimes(1);
    expect(useMissionConnectionStore.getState().status).toBe("disconnected");
  });
});
