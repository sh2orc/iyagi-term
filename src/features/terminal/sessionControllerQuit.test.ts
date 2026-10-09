/**
 * 앱 종료 확인 흐름(quit.rs ↔ sessionController):
 * - 살아 있는 터미널이 없으면 묻지 않고 바로 종료한다.
 * - 있으면 대화상자(modal kind "quit")를 띄우고, Rust 워치독은 먼저 푼다(ack).
 * - "터미널도 종료"는 살아 있는 작업을 전부 취소하고 저장 배치를 비운 뒤
 *   종료하며, "유지"는 배치를 그대로 두고 종료한다.
 * - quitBehavior 설정(keep/terminate)은 대화상자를 건너뛴다.
 * - 취소는 Rust 쪽 대기도 함께 푼다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MockDaemonClient } from "../daemon/mockClient";
import { TerminalRegistry, type FitAddonLike, type RegistryDom, type TerminalLike } from "./registry";
import { SessionController } from "./sessionController";
import { useWorkbenchStore, type WorkbenchState } from "../../store/workbenchStore";
import { DEFAULT_PREFERENCES, usePreferences } from "../../store/preferences";
import type { QuitPort } from "../app/quit";

function fakeTerminal(): TerminalLike {
  return {
    open: () => undefined,
    write: (_data, callback) => {
      if (callback) queueMicrotask(callback);
    },
    resize: () => undefined,
    dispose: () => undefined,
    onData: () => ({ dispose: () => undefined }),
    hasSelection: () => false,
    getSelection: () => "",
    attachCustomKeyEventHandler: () => undefined,
    loadAddon: () => undefined,
    element: null,
  };
}

function fakeQuitPort() {
  const calls: string[] = [];
  const port: QuitPort = {
    ack: async () => {
      calls.push("ack");
    },
    exit: async () => {
      calls.push("exit");
    },
    cancel: async () => {
      calls.push("cancel");
    },
    reveal: async () => {
      calls.push("reveal");
    },
  };
  return { port, calls };
}

function makeStack() {
  let n = 0;
  const mock = new MockDaemonClient({ schedule: (fn) => fn(), resourceIntervalMs: 0, uuid: () => `u${++n}` });
  const registry = new TerminalRegistry({
    createTerminal: fakeTerminal,
    createFitAddon: (): FitAddonLike => ({ proposeDimensions: () => ({ cols: 80, rows: 24 }) }),
    createDom: () => ({ className: "", parentElement: null, remove: () => undefined }) as RegistryDom,
  });
  const quit = fakeQuitPort();
  const controller = new SessionController({ client: mock, registry, platform: "darwin", quit: quit.port });
  const cancel = vi.spyOn(mock, "workloadCancel");
  return { mock, registry, controller, quit, cancel };
}

function resetStore() {
  useWorkbenchStore.setState({
    tabs: [],
    activeTabId: null,
    focusedLeafId: null,
    panes: {},
    workloads: [],
    queue: [],
    host: null,
    revision: 0,
    modal: null,
    toast: null,
  } as Partial<WorkbenchState>);
}

async function liveTerminal(controller: SessionController): Promise<void> {
  controller.newTerminal();
  await vi.waitFor(() => {
    const panes = Object.values(useWorkbenchStore.getState().panes);
    expect(panes.length).toBeGreaterThan(0);
    expect(panes.every((pane) => pane.phase === "live")).toBe(true);
  });
}

describe("quit confirmation", () => {
  beforeEach(() => {
    resetStore();
    usePreferences.setState({ ...DEFAULT_PREFERENCES });
  });
  afterEach(() => {
    usePreferences.setState({ ...DEFAULT_PREFERENCES });
  });

  it("quits immediately (after acking) when nothing is running", async () => {
    const { controller, quit, cancel } = makeStack();
    controller.start();
    controller.requestQuit();
    await vi.waitFor(() => expect(quit.calls).toEqual(["ack", "exit"]));
    expect(useWorkbenchStore.getState().modal).toBeNull();
    expect(cancel).not.toHaveBeenCalled();
    controller.stop();
  });

  it("asks when terminals are live and reports how many", async () => {
    const { controller, quit } = makeStack();
    controller.start();
    await liveTerminal(controller);
    controller.newTab();
    await liveTerminal(controller);

    controller.requestQuit();
    // The window is revealed only when there is a dialog to look at.
    expect(quit.calls).toEqual(["ack", "reveal"]);
    expect(useWorkbenchStore.getState().modal).toEqual({ kind: "quit", sessions: 2 });
    // A second request (tray clicked again) keeps the same dialog.
    controller.requestQuit();
    expect(useWorkbenchStore.getState().modal).toEqual({ kind: "quit", sessions: 2 });
    expect(quit.calls).toEqual(["ack", "reveal", "ack"]);
    controller.stop();
  });

  it("cancel closes the dialog and releases the native side", async () => {
    const { controller, quit } = makeStack();
    controller.start();
    await liveTerminal(controller);
    controller.requestQuit();
    controller.cancelQuit();
    expect(useWorkbenchStore.getState().modal).toBeNull();
    expect(quit.calls).toEqual(["ack", "reveal", "cancel"]);
    // Terminals untouched — the pane is still there for the next session.
    expect(Object.keys(useWorkbenchStore.getState().panes)).toHaveLength(1);
    controller.stop();
  });

  it("keep: exits without cancelling anything and leaves the saved layout alone", async () => {
    const { controller, quit, cancel } = makeStack();
    controller.start();
    await liveTerminal(controller);
    controller.requestQuit();
    await controller.confirmQuit(false);
    expect(cancel).not.toHaveBeenCalled();
    expect(quit.calls).toEqual(["ack", "reveal", "exit"]);
    expect(useWorkbenchStore.getState().modal).toBeNull();
    expect(useWorkbenchStore.getState().tabs).toHaveLength(1);
    controller.stop();
  });

  it("terminate: cancels every live workload, clears the layout, then exits", async () => {
    const { controller, quit, cancel, mock } = makeStack();
    controller.start();
    await liveTerminal(controller);
    controller.newTab();
    await liveTerminal(controller);
    const ids = controller.activeWorkloadIds();
    expect(ids).toHaveLength(2);

    controller.requestQuit();
    await controller.confirmQuit(true);
    expect(cancel).toHaveBeenCalledTimes(2);
    expect(cancel.mock.calls.every(([params]) => params.force === true)).toBe(true);
    expect(cancel.mock.calls.map((call) => call[0].workload_id).sort()).toEqual([...ids].sort());
    expect(quit.calls).toEqual(["ack", "reveal", "exit"]);
    expect(useWorkbenchStore.getState().tabs).toEqual([]);
    expect(useWorkbenchStore.getState().panes).toEqual({});
    expect(useWorkbenchStore.getState().modal).toBeNull();
    // The mock daemon really ended the sessions.
    await vi.waitFor(() => {
      const snapshot = mock.systemSnapshot();
      return snapshot.then((s) => expect(s.workloads.every((w) => w.state === "CANCELLED")).toBe(true));
    });
    controller.stop();
  });

  it("includes running workloads missing from the window's cached state", async () => {
    const { controller, quit, cancel } = makeStack();
    controller.start();
    await liveTerminal(controller);
    controller.requestQuit();
    const ids = controller.activeWorkloadIds();
    useWorkbenchStore.setState({ workloads: [], panes: {} });
    await controller.confirmQuit(true);
    expect(cancel.mock.calls.map(([params]) => params.workload_id)).toEqual(ids);
    expect(quit.calls).toContain("exit");
    controller.stop();
  });

  it("waits for actual termination after the cancel response", async () => {
    const { controller, quit, cancel, mock } = makeStack();
    controller.start();
    await liveTerminal(controller);
    controller.requestQuit();
    const realCancel = mock.workloadCancel.bind(mock);
    cancel.mockResolvedValueOnce({ state: "STOPPING" });
    const decision = controller.confirmQuit(true);
    await vi.waitFor(() => expect(cancel).toHaveBeenCalledOnce());
    expect(quit.calls).not.toContain("exit");
    expect(useWorkbenchStore.getState().tabs).toHaveLength(1);
    await realCancel(cancel.mock.calls[0][0]);
    await decision;
    expect(quit.calls).toContain("exit");
    controller.stop();
  });

  it("keeps the app and layout open if force cancellation fails", async () => {
    const { controller, quit, cancel } = makeStack();
    controller.start();
    await liveTerminal(controller);
    controller.requestQuit();
    cancel.mockRejectedValueOnce(new Error("connection lost"));
    await controller.confirmQuit(true);
    expect(quit.calls).not.toContain("exit");
    expect(useWorkbenchStore.getState().tabs).toHaveLength(1);
    expect(useWorkbenchStore.getState().modal?.kind).toBe("quit");
    expect(useWorkbenchStore.getState().toast).not.toBeNull();
    // Retry can succeed after a transient connection failure.
    await controller.confirmQuit(true);
    expect(quit.calls).toContain("exit");
    controller.stop();
  });

  it("confirmQuit is single-flight (double click exits once)", async () => {
    const { controller, quit } = makeStack();
    controller.start();
    await liveTerminal(controller);
    controller.requestQuit();
    const first = controller.confirmQuit(false);
    const second = controller.confirmQuit(false);
    expect(second).toBe(first);
    await first;
    expect(quit.calls.filter((call) => call === "exit")).toHaveLength(1);
    controller.stop();
  });

  it("quitBehavior=keep skips the dialog", async () => {
    const { controller, quit, cancel } = makeStack();
    usePreferences.getState().setQuitBehavior("keep");
    controller.start();
    await liveTerminal(controller);
    controller.requestQuit();
    await vi.waitFor(() => expect(quit.calls).toEqual(["ack", "exit"]));
    expect(useWorkbenchStore.getState().modal).toBeNull();
    expect(cancel).not.toHaveBeenCalled();
    controller.stop();
  });

  it("quitBehavior=terminate skips the dialog and terminates", async () => {
    const { controller, quit, cancel } = makeStack();
    usePreferences.getState().setQuitBehavior("terminate");
    controller.start();
    await liveTerminal(controller);
    controller.requestQuit();
    await vi.waitFor(() => expect(quit.calls).toEqual(["ack", "exit"]));
    expect(cancel).toHaveBeenCalledOnce();
    expect(useWorkbenchStore.getState().panes).toEqual({});
    controller.stop();
  });

  it("counts detached (pane-less) running workloads too", async () => {
    const { controller } = makeStack();
    controller.start();
    await liveTerminal(controller);
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    // Close the pane but keep the process (창만 닫기) — it still runs.
    await controller.confirmClosePanes([pane.leafId], false, useWorkbenchStore.getState().activeTabId!);
    expect(Object.keys(useWorkbenchStore.getState().panes)).toHaveLength(0);
    expect(controller.activeWorkloadIds()).toEqual([pane.workloadId]);
    controller.requestQuit();
    expect(useWorkbenchStore.getState().modal).toEqual({ kind: "quit", sessions: 1 });
    controller.cancelQuit();
    controller.stop();
  });

  it("works without a quit port (browser preview) — never throws", async () => {
    let n = 0;
    const mock = new MockDaemonClient({ schedule: (fn) => fn(), resourceIntervalMs: 0, uuid: () => `p${++n}` });
    const registry = new TerminalRegistry({
      createTerminal: fakeTerminal,
      createDom: () => ({ className: "", parentElement: null, remove: () => undefined }) as RegistryDom,
    });
    const controller = new SessionController({ client: mock, registry, platform: "linux" });
    controller.start();
    expect(() => controller.requestQuit()).not.toThrow();
    await expect(controller.confirmQuit(false)).resolves.toBeUndefined();
    expect(() => controller.cancelQuit()).not.toThrow();
    controller.stop();
  });
});

describe("quitPending", () => {
  beforeEach(() => {
    resetStore();
    usePreferences.setState({ ...DEFAULT_PREFERENCES });
  });

  it("is true only while a decision is in flight (backdrop dismiss must not cancel it)", async () => {
    const { controller } = makeStack();
    controller.start();
    await liveTerminal(controller);
    controller.requestQuit();
    expect(controller.quitPending).toBe(false);
    const decision = controller.confirmQuit(false);
    expect(controller.quitPending).toBe(true);
    await decision;
    expect(controller.quitPending).toBe(false);
    controller.stop();
  });
});
