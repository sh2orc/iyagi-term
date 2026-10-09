import { afterEach, describe, expect, it, vi } from "vitest";
import type { DaemonClient } from "../daemon/client";
import { SessionController } from "./sessionController";
import { TerminalRegistry, type RegistryDom, type TerminalLike } from "./registry";

function pendingClient(disposeEvent: () => void): DaemonClient {
  return {
    events: { subscribe: () => ({ dispose: disposeEvent }) },
    systemSnapshot: () => new Promise(() => undefined),
  } as unknown as DaemonClient;
}

function fakeDom(): RegistryDom {
  return { className: "", parentElement: null, remove: () => undefined };
}

describe("SessionController React lifecycle", () => {
  afterEach(() => vi.useRealTimers());

  it("cancels deferred teardown during StrictMode's immediate remount", () => {
    vi.useFakeTimers();
    const disposeTerminal = vi.fn();
    const disposeEvent = vi.fn();
    const registry = new TerminalRegistry({
      createTerminal: () => ({
        open: () => undefined,
        write: () => undefined,
        resize: () => undefined,
        dispose: disposeTerminal,
        onData: () => ({ dispose: () => undefined }),
        hasSelection: () => false,
        getSelection: () => "",
        attachCustomKeyEventHandler: () => undefined,
        loadAddon: () => undefined,
        element: null,
      } satisfies TerminalLike),
      createDom: fakeDom,
    });
    registry.acquire("view-1");
    const controller = new SessionController({
      client: pendingClient(disposeEvent),
      registry,
      platform: "darwin",
    });

    controller.start();
    controller.scheduleDispose();
    controller.start();
    vi.runAllTimers();
    expect(registry.liveCount).toBe(1);
    expect(disposeTerminal).not.toHaveBeenCalled();

    controller.scheduleDispose();
    vi.runAllTimers();
    expect(registry.liveCount).toBe(0);
    expect(disposeTerminal).toHaveBeenCalledTimes(1);
    expect(disposeEvent).toHaveBeenCalledTimes(2);
  });
});
