/**
 * End-to-end stack test: SessionController + TerminalRegistry(fakes) +
 * SessionPipeline + MockDaemonClient.
 * Proves: single launch per pane, replay→live transition, echo round-trip
 * into the terminal, resize request → journal-ordered term.resize, and
 * StrictMode-safe mount lifecycle.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MockDaemonClient } from "../src/features/daemon/mockClient";
import { TerminalRegistry, type FitAddonLike, type RegistryDom, type TerminalLike } from "../src/features/terminal/registry";
import { encodeWorkspace, decodeWorkspace } from "../src/store/workspaceStorage";
import { SessionController, groupOrphansByCwd } from "../src/features/terminal/sessionController";
import { leafCount } from "../src/features/terminal/splitTree";
import { useWorkbenchStore, type WorkbenchState } from "../src/store/workbenchStore";
import { usePreferences } from "../src/store/preferences";
import { useNotificationStore } from "../src/features/notifications/notificationStore";
import { base64ToBytes } from "../src/features/daemon/base64";
import { layoutRoster } from "../src/features/terminal/layoutRoster";
import { t } from "../src/i18n";

class StackTerminal implements TerminalLike {
  order: string[] = [];
  writes: Uint8Array[] = [];
  resizes: Array<{ cols: number; rows: number }> = [];
  dataCb: ((data: string) => void) | null = null;
  disposed = false;

  open(): void {
    this.order.push("open");
  }
  /** Real xterm consumes writes asynchronously; emulate with a microtask. */
  write(data: Uint8Array | string, callback?: () => void): void {
    const bytes = typeof data === "string" ? new TextEncoder().encode(data) : data;
    this.writes.push(bytes);
    this.order.push(`write:${new TextDecoder().decode(bytes)}`);
    if (callback) queueMicrotask(() => callback());
  }
  resize(cols: number, rows: number): void {
    this.resizes.push({ cols, rows });
    this.order.push(`resize:${cols}x${rows}`);
  }
  dispose(): void {
    this.disposed = true;
  }
  onData(callback: (data: string) => void): { dispose(): void } {
    this.dataCb = callback;
    return { dispose: () => (this.dataCb = null) };
  }
  hasSelection(): boolean {
    return false;
  }
  getSelection(): string {
    return "";
  }
  attachCustomKeyEventHandler(): void {
    // no-op
  }
  loadAddon(): void {
    // no-op
  }
  element: unknown = null;
  /** xterm 6 `modes` — 붙여넣기 괄호 여부는 이 값을 따른다. */
  modes = { bracketedPasteMode: false };
}

function makeStack(fitDims = { cols: 80, rows: 24 }) {
  const terminals: StackTerminal[] = [];
  const fitCalls: Array<{ cols: number; rows: number }> = [];
  let n = 0;
  const mock = new MockDaemonClient({
    schedule: (fn) => fn(),
    resourceIntervalMs: 0,
    uuid: () => `u${++n}`,
  });
  const registry = new TerminalRegistry({
    createTerminal: () => {
      const term = new StackTerminal();
      terminals.push(term);
      return term;
    },
    createFitAddon: (): FitAddonLike => ({
      proposeDimensions: () => ({ ...fitDims }),
    }),
    createDom: () =>
      ({
        className: "",
        parentElement: null,
        remove: () => undefined,
      }) as RegistryDom,
  });
  const controller = new SessionController({ client: mock, registry, platform: "windows" });
  const launchSpy = vi.spyOn(mock, "workloadLaunch");
  const detachSpy = vi.spyOn(mock, "sessionDetach");
  return { mock, registry, controller, terminals, launchSpy, detachSpy, fitCalls };
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
    workloadMemory: {},
    queueDrawerOpen: false,
    graphDrawerOpen: false,
    modal: null,
    toast: null,
    renamingTabId: null,
    broadcastInput: false,
  } as Partial<WorkbenchState>);
}

describe("controller end-to-end (mock daemon)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetStore();
    usePreferences.setState({ terminateOnClose: false });
  });
  afterEach(() => {
    vi.useRealTimers();
  });


  /**
   * 정상 종료(04-ui §5-1): 사용자가 직접 쓰는 셸이 코드 0으로 끝나면
   * "새 세션으로 다시 시작" 오버레이를 띄우지 않고 그 창을 닫는다.
   */
  it("closes the pane (and the emptied tab) when a shell exits cleanly", async () => {
    const { mock, controller, registry } = makeStack();
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    mock.killSession(pane.sessionId!, 0);
    await vi.waitFor(() => expect(useWorkbenchStore.getState().panes[pane.leafId]).toBeUndefined());
    // 빈 탭을 남기지 않고(마지막 창이었다), 새 셸을 대신 띄우지도 않는다.
    expect(useWorkbenchStore.getState().tabs).toHaveLength(0);
    controller.stop(); registry.disposeAll(); mock.dispose();
  });

  it("keeps the tab when a cleanly exited shell had siblings", async () => {
    const { mock, controller, registry } = makeStack();
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    expect(controller.splitFocused("row")).toBe(true);
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)).toHaveLength(2));
    await vi.waitFor(() =>
      expect(Object.values(useWorkbenchStore.getState().panes).every(p => p.phase === "live")).toBe(true));
    const [first, second] = Object.values(useWorkbenchStore.getState().panes);
    mock.killSession(first.sessionId!, 0);
    await vi.waitFor(() => expect(useWorkbenchStore.getState().panes[first.leafId]).toBeUndefined());
    expect(useWorkbenchStore.getState().panes[second.leafId].phase).toBe("live");
    expect(useWorkbenchStore.getState().tabs).toHaveLength(1);
    controller.stop(); registry.disposeAll(); mock.dispose();
  });

  it("keeps a cleanly exited managed run on screen", async () => {
    const { mock, controller, registry } = makeStack();
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    // 관리 실행은 사용자가 보려고 돌린 결과라 성공해도 창을 지우지 않는다
    // (모크의 managed 런치는 대기열을 거치므로 요약의 mode만 그 값으로 세운다).
    useWorkbenchStore.setState(s => ({
      workloads: s.workloads.map(w => (w.workload_id === pane.workloadId ? { ...w, mode: "managed" as const } : w)),
    }));
    mock.killSession(pane.sessionId!, 0);
    await vi.waitFor(() => expect(useWorkbenchStore.getState().panes[pane.leafId]?.phase).toBe("exited"));
    await vi.advanceTimersByTimeAsync(100);
    expect(useWorkbenchStore.getState().panes[pane.leafId]?.phase).toBe("exited");
    controller.stop(); registry.disposeAll(); mock.dispose();
  });

  it("restarts an exited pane with a fresh writable session and terminal", async () => {
    const { mock, controller, registry, terminals, launchSpy } = makeStack();
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const before = Object.values(useWorkbenchStore.getState().panes)[0];
    await controller.cancelWorkload(before.workloadId!);
    await vi.waitFor(() => expect(useWorkbenchStore.getState().panes[before.leafId].phase).toBe("exited"));
    controller.retryPane(before.leafId);
    controller.retryPane(before.leafId); // double click must not duplicate the launch
    await vi.waitFor(() => expect(useWorkbenchStore.getState().panes[before.leafId].phase).toBe("live"));
    const after = useWorkbenchStore.getState().panes[before.leafId];
    expect(after.sessionId).not.toBe(before.sessionId);
    expect(after.workloadId).not.toBe(before.workloadId);
    expect(after.viewId).not.toBe(before.viewId);
    expect(terminals[0].disposed).toBe(true);
    expect(launchSpy).toHaveBeenCalledTimes(2);
    // 재실행한 새 작업이 끝난 작업을 이어받아 최근 종료에서 빠진다.
    expect(useWorkbenchStore.getState().workloadMemory[before.workloadId!]?.recoveredBy).toBe(after.workloadId);
    const terminal = terminals[terminals.length - 1];
    terminal.dataCb?.("fresh-session-input");
    await vi.waitFor(() => expect(terminal.writes.map(b => new TextDecoder().decode(b)).join("")).toContain("fresh-session-input"));
    controller.stop(); registry.disposeAll(); mock.dispose();
  });

  it.each([
    ["attach reports exited", false],
    // A daemon built before AttachResult.exited never says so; the finished workload state must still decide.
    ["attach omits exited", true],
  ] as const)("connecting a finished ordinary terminal replays its output, then continues it in a fresh shell (%s)", async (_name, omitExited) => {
    const { mock, controller, registry, terminals, launchSpy } = makeStack();
    if (omitExited) {
      const attach = mock.sessionAttach.bind(mock);
      mock.sessionAttach = async (params) => {
        const reply = await attach(params);
        delete reply.exited;
        return reply;
      };
    }
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const before = Object.values(useWorkbenchStore.getState().panes)[0];
    mock.emitProgramOutput(before.sessionId!, new TextEncoder().encode("saved ordinary terminal output\r\n"));
    await vi.waitFor(() => expect(terminals[0].writes.map(b => new TextDecoder().decode(b)).join("")).toContain("saved ordinary terminal output"));
    mock.killSession(before.sessionId!, 1); // 정상 종료(0)는 창을 닫으므로(04-ui §5-1) 끝난 창이 남는 값으로 준다.
    await vi.waitFor(() => expect(useWorkbenchStore.getState().panes[before.leafId].phase).toBe("exited"));
    useWorkbenchStore.getState().applyClose(before.leafId);
    await controller.attachWorkloadTerminal(before.workloadId!);
    // 복구 중에 한 번 더 눌러도 같은 창으로 갈 뿐 두 번 되살리지 않는다.
    await controller.attachWorkloadTerminal(before.workloadId!);
    // 같은 창에 이전 출력을 재생한 뒤, 원래 경로에서 새 셸로 잇는다.
    await vi.waitFor(() => {
      const pane = Object.values(useWorkbenchStore.getState().panes)[0];
      expect(pane?.phase).toBe("live");
      expect(pane?.sessionId).not.toBe(before.sessionId);
    });
    expect(Object.keys(useWorkbenchStore.getState().panes)).toHaveLength(1);
    const restored = Object.values(useWorkbenchStore.getState().panes)[0];
    expect(restored.workloadId).not.toBe(before.workloadId);
    expect(launchSpy).toHaveBeenCalledTimes(2);
    expect(launchSpy.mock.calls[1][0].cwd).toBe(launchSpy.mock.calls[0][0].cwd);
    // 재생한 화면을 그대로 두고 그 아래에서 잇는다: 새 터미널을 만들지 않는다.
    expect(terminals).toHaveLength(2);
    const terminal = terminals[1];
    expect(registry.get(restored.viewId)?.terminal).toBe(terminal);
    const screen = terminal.writes.map(b => new TextDecoder().decode(b)).join("");
    const replayed = screen.indexOf("saved ordinary terminal output");
    expect(replayed).toBeGreaterThanOrEqual(0);
    expect(screen.indexOf(t("terminal.recover.separator"))).toBeGreaterThan(replayed);
    // 새 셸은 관리 작업(살아 있는 작업)이 되고, 끝난 작업은 최근 종료에서 빠진다.
    expect((await mock.systemSnapshot()).workloads.find(w => w.workload_id === restored.workloadId)?.state).toBe("RUNNING");
    expect(useWorkbenchStore.getState().workloadMemory[before.workloadId!]?.recoveredBy).toBe(restored.workloadId);
    terminal.dataCb?.("recovered-shell-input");
    await vi.waitFor(() => expect(terminal.writes.map(b => new TextDecoder().decode(b)).join("")).toContain("recovered-shell-input"));
    controller.dispose(); registry.disposeAll(); mock.dispose();
  });

  it("a finished pane restored after an app reload stays finished without attach exited, then connecting continues it in place", async () => {
    const { mock, controller, registry, launchSpy } = makeStack();
    // A daemon built before AttachResult.exited: attaching a finished session never says so.
    const attach = mock.sessionAttach.bind(mock);
    mock.sessionAttach = async (params) => {
      const reply = await attach(params);
      delete reply.exited;
      return reply;
    };
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const before = Object.values(useWorkbenchStore.getState().panes)[0];
    mock.emitProgramOutput(before.sessionId!, new TextEncoder().encode("output before the reload\r\n"));
    mock.killSession(before.sessionId!, 1); // 정상 종료(0)는 창을 닫으므로(04-ui §5-1) 끝난 창이 남는 값으로 준다.
    await vi.waitFor(() => expect(useWorkbenchStore.getState().panes[before.leafId].phase).toBe("exited"));
    // The app reloads while the finished pane is still open.
    const saved = encodeWorkspace(useWorkbenchStore.getState());
    controller.stop();
    registry.disposeAll();
    resetStore();
    useWorkbenchStore.setState(decodeWorkspace(saved)!);
    const terminals: StackTerminal[] = [];
    const restoredRegistry = new TerminalRegistry({
      createTerminal: () => {
        const term = new StackTerminal();
        terminals.push(term);
        return term;
      },
      createDom: () => ({ className: "", parentElement: null, remove: () => undefined }),
    });
    const restored = new SessionController({ client: mock, registry: restoredRegistry, platform: "windows" });
    const inputSpy = vi.spyOn(mock, "sessionInput");
    restored.start();
    await restored.restoreWorkspace();
    // Its journal replays, but the dead PTY must not look writable.
    await vi.waitFor(() => expect(useWorkbenchStore.getState().panes[before.leafId]?.phase).toBe("exited"));
    const screen = () => terminals.map(term => term.writes.map(b => new TextDecoder().decode(b)).join("")).join("");
    expect(screen()).toContain("output before the reload");
    terminals[0].dataCb?.("typed-into-finished-pane");
    await vi.advanceTimersByTimeAsync(100);
    expect(inputSpy).not.toHaveBeenCalled();
    // 최근 종료의 터미널 연결: 같은 창에서 이전 출력 아래 새 셸로 잇는다.
    await restored.attachWorkloadTerminal(before.workloadId!);
    await vi.waitFor(() => {
      const pane = useWorkbenchStore.getState().panes[before.leafId];
      expect(pane?.phase).toBe("live");
      expect(pane?.sessionId).not.toBe(before.sessionId);
    });
    const after = useWorkbenchStore.getState().panes[before.leafId];
    expect(Object.keys(useWorkbenchStore.getState().panes)).toEqual([before.leafId]);
    expect(launchSpy).toHaveBeenCalledTimes(2);
    expect(screen().indexOf(t("terminal.recover.separator"))).toBeGreaterThan(screen().indexOf("output before the reload"));
    expect(useWorkbenchStore.getState().workloadMemory[before.workloadId!]?.recoveredBy).toBe(after.workloadId);
    restored.dispose(); restoredRegistry.disposeAll(); mock.dispose();
  });

  it("asks before closing, allows cancel and detach, then reconnects without relaunching", async () => {
    const { mock, controller, registry, launchSpy } = makeStack();
    const cancel = vi.spyOn(mock, "workloadCancel");
    controller.start(); controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    controller.requestClosePanes([pane.leafId]);
    expect(useWorkbenchStore.getState().modal).toMatchObject({ kind: "close-panes", leafIds: [pane.leafId] });
    expect(cancel).not.toHaveBeenCalled();
    useWorkbenchStore.getState().closeModal();
    expect(useWorkbenchStore.getState().panes[pane.leafId]).toBeDefined();
    await controller.confirmClosePanes([pane.leafId], false, useWorkbenchStore.getState().activeTabId!);
    expect(cancel).not.toHaveBeenCalled();
    expect(useWorkbenchStore.getState().tabs).toHaveLength(0);
    controller.attachWorkloadTerminal(pane.workloadId!);
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    expect(Object.values(useWorkbenchStore.getState().panes)[0].sessionId).toBe(pane.sessionId);
    expect(launchSpy).toHaveBeenCalledOnce();
    usePreferences.getState().setTerminateOnClose(true);
    controller.requestClosePanes([Object.keys(useWorkbenchStore.getState().panes)[0]]);
    await vi.waitFor(() => expect(cancel).toHaveBeenCalledOnce());
    expect(useWorkbenchStore.getState().modal).toBeNull();
    controller.stop(); registry.disposeAll(); mock.dispose();
  });

  it("always asks before closing every tab and applies the selected termination policy to all panes", async () => {
    const { mock, controller, registry } = makeStack();
    const cancel = vi.spyOn(mock, "workloadCancel");
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)).toHaveLength(1));
    controller.newTab();
    controller.newTerminal();
    await vi.waitFor(() => {
      expect(Object.values(useWorkbenchStore.getState().panes)).toHaveLength(2);
      expect(Object.values(useWorkbenchStore.getState().panes).every((pane) => pane.phase === "live")).toBe(true);
    });

    usePreferences.getState().setTerminateOnClose(true);
    controller.requestCloseAllTabs();
    const modal = useWorkbenchStore.getState().modal;
    expect(modal).toMatchObject({ kind: "close-panes", closeAllTabs: true });
    expect(modal?.kind === "close-panes" ? modal.leafIds : []).toHaveLength(2);
    expect(cancel).not.toHaveBeenCalled();

    useWorkbenchStore.getState().closeModal();
    expect(useWorkbenchStore.getState().tabs).toHaveLength(2);
    expect(useWorkbenchStore.getState().panes).not.toEqual({});

    controller.requestCloseAllTabs();
    const confirmation = useWorkbenchStore.getState().modal;
    expect(confirmation?.kind).toBe("close-panes");
    if (confirmation?.kind !== "close-panes") throw new Error("close-all confirmation was not opened");
    useWorkbenchStore.getState().closeModal();
    await controller.confirmClosePanes(confirmation.leafIds, true, undefined, true);

    expect(cancel).toHaveBeenCalledTimes(2);
    expect(useWorkbenchStore.getState().tabs).toHaveLength(0);
    expect(useWorkbenchStore.getState().panes).toEqual({});
    controller.stop(); registry.disposeAll(); mock.dispose();
  });

  it("launch → attach → replay → live; echo reaches the terminal; StrictMode-safe", async () => {
    const { mock, registry, controller, terminals, launchSpy, detachSpy } = makeStack();
    controller.start();
    controller.newTab(); // Workbench effect가 하는 초기 탭 생성을 모방
    expect(useWorkbenchStore.getState().tabs.length).toBe(1);

    controller.newTerminal(); // 첫 pane 생성+셸 시작(사용자 동작 — effect 밖)
    await vi.waitFor(() => {
      const panes = Object.values(useWorkbenchStore.getState().panes);
      expect(panes.length).toBe(1);
      expect(panes[0].phase).toBe("live");
    });
    expect(launchSpy).toHaveBeenCalledOnce(); // 단 한 번의 세션 시작

    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    expect(pane.sessionId).not.toBeNull();
    const term = terminals[0];
    expect(term.disposed).toBe(false);

    // Registry mount는 DOM만 담당 — mount/unmount 후에도 세션은 살아 있다(U09).
    const host = { children: [] as unknown[], appendChild: (c: unknown) => host.children.push(c) };
    const remount = () => {
      const entry = registry.acquire(pane.viewId);
      entry.mount(host);
      return () => entry.unmount();
    };
    const cleanup = remount();
    cleanup();
    remount();
    expect(launchSpy).toHaveBeenCalledOnce();
    expect(detachSpy).not.toHaveBeenCalled();

    // echo round-trip: 키보드 입력 → daemon echo → terminal write.
    const before = term.writes.length;
    term.dataCb?.("echo-me\r");
    await vi.waitFor(() => expect(term.writes.length).toBeGreaterThan(before));
    const last = term.writes[term.writes.length - 1];
    expect(new TextDecoder().decode(last)).toBe("echo-me\r");

    // ACK가 flush 되었는지(16ms 배치).
    vi.advanceTimersByTime(20);
    const inspect = mock.inspect(pane.sessionId as string);
    expect(inspect.ackedSeq).toBeGreaterThan(0);

    mock.dispose();
  });

  it("sends a resize request from fit dimensions and applies it via the journal record", async () => {
    const { mock, registry, controller, terminals } = makeStack({ cols: 120, rows: 40 });
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => {
      const panes = Object.values(useWorkbenchStore.getState().panes);
      expect(panes[0]?.phase).toBe("live");
    });
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    const host = { children: [] as unknown[], appendChild: (c: unknown) => host.children.push(c) };
    registry.acquire(pane.viewId).mount(host); // mount → refit → requestResize(120x40)
    await vi.advanceTimersByTimeAsync(20); // 16ms coalesce

    // 첫 record는 initial size(80x24), 이후 fit이 요청한 120x40가 journal 순서로 적용된다.
    expect(terminals[0].resizes[0]).toEqual({ cols: 80, rows: 24 });
    await vi.waitFor(() =>
      expect(terminals[0].resizes.some((r) => r.cols === 120 && r.rows === 40)).toBe(true),
    );
    const inspect = mock.inspect(pane.sessionId as string);
    expect(inspect.recordCount).toBeGreaterThanOrEqual(3); // initial size, banner, resize

    mock.dispose();
  });

  it.each(["launch", "attach"] as const)("fills an already mounted pane after delayed %s without a window resize", async (delay) => {
    const { mock, registry, controller, terminals } = makeStack({ cols: 120, rows: 48 });
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    if (delay === "launch") {
      const launch = mock.workloadLaunch.bind(mock);
      vi.spyOn(mock, "workloadLaunch").mockImplementation(async (request) => {
        await gate;
        return launch(request);
      });
    } else {
      const attach = mock.sessionAttach.bind(mock);
      vi.spyOn(mock, "sessionAttach").mockImplementation(async (request) => {
        await gate;
        return attach(request);
      });
    }
    controller.start();
    controller.newTerminal();
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    registry.acquire(pane.viewId).mount({ appendChild: () => undefined });
    // Initial mount/fit finishes before the native launch or attach replies.
    await vi.advanceTimersByTimeAsync(100);
    release();
    await vi.advanceTimersByTimeAsync(100);
    expect(terminals[0].resizes.at(-1)).toEqual({ cols: 120, rows: 48 });
    controller.stop();
    registry.disposeAll();
    mock.dispose();
  });

  it("restores output and fit when a new controller mounts existing panes", async () => {
    const { mock, registry, controller, launchSpy } = makeStack();
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    controller.stop();
    registry.disposeAll();

    const restoredTerminal = new StackTerminal();
    const restoredRegistry = new TerminalRegistry({
      createTerminal: () => restoredTerminal,
      createFitAddon: () => ({ proposeDimensions: () => ({ cols: 120, rows: 48 }) }),
      createDom: () => ({ className: "", parentElement: null, remove: () => undefined }),
    });
    restoredRegistry.acquire(pane.viewId).mount({ appendChild: () => undefined });
    const restored = new SessionController({ client: mock, registry: restoredRegistry, platform: "windows" });
    const attachSpy = vi.spyOn(mock, "sessionAttach");
    restored.start();
    restored.stop();
    restored.start(); // StrictMode must not attach twice.
    await vi.advanceTimersByTimeAsync(100);
    expect(attachSpy).toHaveBeenCalledOnce();
    expect(launchSpy).toHaveBeenCalledOnce();
    expect(restoredTerminal.writes.length).toBeGreaterThan(0);
    expect(restoredTerminal.resizes.at(-1)).toEqual({ cols: 120, rows: 48 });
    const before = restoredTerminal.writes.length;
    restoredTerminal.dataCb?.("restored-input\r");
    await vi.advanceTimersByTimeAsync(100);
    expect(restoredTerminal.writes.length).toBeGreaterThan(before);
    restored.stop();
    restoredRegistry.disposeAll();
    mock.dispose();
  });

  it.each([true, false])("reopens live daemon sessions without launching shells (saved layout: %s)", async (saveLayout) => {
    const { mock, registry, controller, launchSpy } = makeStack();
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    controller.dispatchShortcut("split-row");
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes).filter(p => p.phase === "live")).toHaveLength(2));
    const before = useWorkbenchStore.getState();
    const saved = encodeWorkspace(before);
    const sessionIds = Object.values(before.panes).map(p => p.sessionId).sort();
    controller.stop();
    registry.disposeAll();
    resetStore();
    if (saveLayout) useWorkbenchStore.setState(decodeWorkspace(saved)!);
    const restoredRegistry = new TerminalRegistry({ createTerminal: () => new StackTerminal(), createDom: () => ({className: "", parentElement: null, remove: () => undefined}) });
    const restored = new SessionController({client: mock, registry: restoredRegistry, platform: "windows"});
    const attachSpy = vi.spyOn(mock, "sessionAttach");
    restored.start();
    restored.stop();
    restored.start();
    await restored.restoreWorkspace();
    await vi.advanceTimersByTimeAsync(100);
    expect(launchSpy).toHaveBeenCalledTimes(2); // Only the original two launches.
    expect(attachSpy).toHaveBeenCalledTimes(2);
    expect(Object.values(useWorkbenchStore.getState().panes).map(p => p.sessionId).sort()).toEqual(sessionIds);
    if (saveLayout) expect(useWorkbenchStore.getState().tabs).toEqual(before.tabs);
    expect(Object.values(useWorkbenchStore.getState().panes).every(p => p.phase === "live")).toBe(true);
    restored.stop();
    restoredRegistry.disposeAll();
    mock.dispose();
  });

  it("split focuses the new pane, keeps the existing session, and enforces the 8-pane cap message", async () => {
    const { mock, controller, launchSpy } = makeStack();
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const firstSessionId = Object.values(useWorkbenchStore.getState().panes)[0].sessionId;

    controller.dispatchShortcut("split-row");
    await vi.waitFor(() => expect(Object.keys(useWorkbenchStore.getState().panes).length).toBe(2));
    await vi.waitFor(() => expect(launchSpy).toHaveBeenCalledTimes(2));

    // 기존 leaf/session은 first에 그대로 유지되고 새 pane이 focus를 갖는다.
    const state = useWorkbenchStore.getState();
    const root = state.tabs.find((t) => t.id === state.activeTabId)?.root;
    expect(root && root.kind === "split" && root.first.kind === "leaf" && root.first.session_id).toBe(
      firstSessionId,
    );
    const panes = Object.values(state.panes);
    const focused = panes.find((p) => p.leafId === state.focusedLeafId);
    expect(focused).toBeDefined();
    expect(focused?.sessionId).not.toBe(firstSessionId); // 새로운 session
    await vi.waitFor(() => expect(focused?.phase === "live" || focused?.phase === "replaying" || focused?.phase === "starting").toBe(true));

    // pane 닫기 = 세션 종료(04 §2): 닫은 pane의 workload만 끝나고 남은 pane은 살아 있다.
    const secondLeaf = useWorkbenchStore.getState().focusedLeafId as string;
    const closedWorkloadId = useWorkbenchStore.getState().panes[secondLeaf].workloadId;
    expect(closedWorkloadId).toBeTruthy();
    await controller.closePane(secondLeaf);
    expect(Object.keys(useWorkbenchStore.getState().panes).length).toBe(1);

    const snapshot = await mock.systemSnapshot();
    const closed = snapshot.workloads.find((w) => w.workload_id === closedWorkloadId);
    expect(closed).toBeDefined();
    expect(["STOPPING", "DRAINING", "CANCELLED", "SUCCEEDED", "FAILED", "INTERRUPTED"]).toContain(closed?.state);
    const survivorId = Object.values(useWorkbenchStore.getState().panes)[0].workloadId;
    const survivor = snapshot.workloads.find((w) => w.workload_id === survivorId);
    expect(["STARTING", "RUNNING"]).toContain(survivor?.state);

    mock.dispose();
  });

  it("keeps a pending launch alive when closing only its pane", async () => {
    const { mock, controller, registry } = makeStack();
    const cancel = vi.spyOn(mock, "workloadCancel");
    controller.start(); controller.newTerminal();
    const leafId = Object.keys(useWorkbenchStore.getState().panes)[0];
    await controller.closePane(leafId, false);
    await vi.waitFor(async () => expect((await mock.systemSnapshot()).workloads).toHaveLength(1));
    expect(cancel).not.toHaveBeenCalled();
    const workload = (await mock.systemSnapshot()).workloads[0];
    controller.attachWorkloadTerminal(workload.workload_id);
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    controller.stop(); registry.disposeAll(); mock.dispose();
  });

  it("시작 도중에 닫아도 방금 만들어진 session을 남기지 않는다", async () => {
    const { mock, controller } = makeStack();
    controller.start();
    controller.newTerminal(); // pane은 즉시 생기고 launch는 아직 진행 중이다.
    const leafId = Object.keys(useWorkbenchStore.getState().panes)[0];
    expect(useWorkbenchStore.getState().panes[leafId].workloadId).toBeNull(); // 아직 launch 응답 전

    await controller.closePane(leafId); // 응답을 기다리지 않고 닫는다
    expect(useWorkbenchStore.getState().panes[leafId]).toBeUndefined();

    // 뒤늦게 도착한 launch 결과의 session도 함께 종료돼야 한다(고아 세션 금지).
    await vi.waitFor(async () => {
      const workloads = (await mock.systemSnapshot()).workloads;
      expect(workloads.length).toBeGreaterThanOrEqual(1);
      for (const workload of workloads) {
        expect(["STARTING", "RUNNING", "QUEUED"]).not.toContain(workload.state);
      }
    });

    mock.dispose();
  });

  it("rejects split when the pane is too small and shows the exact message", async () => {
    const { mock, controller } = makeStack();
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));

    // pane 치수를 최소 이하로 주입한 뒤 분할 시도 → 거절 + 문구.
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    const internal = (controller as unknown as { paneSizes: Map<string, { width: number; height: number }> }).paneSizes;
    internal.set(pane.viewId, { width: 300, height: 400 });
    const before = Object.keys(useWorkbenchStore.getState().panes).length;
    controller.dispatchShortcut("split-row");
    expect(Object.keys(useWorkbenchStore.getState().panes).length).toBe(before);
    expect(useWorkbenchStore.getState().toast).toBe("분할할 공간이 부족합니다");

    mock.dispose();
  });
});

describe("broadcast input (동기 입력)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetStore();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  /** 두 pane을 live 상태로 만들고 [원본 pane, 형제 pane] 순으로 돌려준다. */
  async function twoLivePanes() {
    const stack = makeStack();
    stack.controller.start();
    stack.controller.newTab();
    stack.controller.newTerminal();
    await vi.waitFor(() => {
      expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live");
    });
    stack.controller.splitFocused("row");
    await vi.waitFor(() => {
      const panes = Object.values(useWorkbenchStore.getState().panes);
      expect(panes.length).toBe(2);
      expect(panes.every((p) => p.phase === "live")).toBe(true);
    });
    // splitFocused는 새 pane으로 focus를 옮긴다 — 입력 원본은 그 pane이다.
    const focusedLeaf = useWorkbenchStore.getState().focusedLeafId as string;
    const origin = useWorkbenchStore.getState().panes[focusedLeaf];
    const sibling = Object.values(useWorkbenchStore.getState().panes).find(
      (p) => p.leafId !== focusedLeaf,
    );
    const terminalOf = (viewId: string) => stack.registry.get(viewId)?.terminal as StackTerminal;
    return { ...stack, origin, sibling: sibling!, terminalOf };
  }

  function inputsFor(spy: ReturnType<typeof vi.spyOn>, sessionId: string): string[] {
    return spy.mock.calls
      .map(([params]) => params as { session_id: string; data_b64: string })
      .filter((p) => p.session_id === sessionId)
      .map((p) => new TextDecoder().decode(base64ToBytes(p.data_b64)));
  }

  it("toggles from the keymap action without stacking a toast on the banner", () => {
    const { mock, controller } = makeStack();
    controller.start();
    controller.dispatchShortcut("broadcast-toggle");
    expect(useWorkbenchStore.getState().broadcastInput).toBe(true);
    // 켜진 상태는 띠와 pane 테두리가 상시로 알린다 — toast로 겹쳐 말하지 않는다.
    expect(useWorkbenchStore.getState().toast).toBeNull();
    controller.dispatchShortcut("broadcast-toggle");
    expect(useWorkbenchStore.getState().broadcastInput).toBe(false);
    controller.stop();
    mock.dispose();
  });

  it("keeps input in the origin pane while broadcast is off", async () => {
    const { mock, controller, origin, sibling, terminalOf } = await twoLivePanes();
    const inputSpy = vi.spyOn(mock, "sessionInput");
    terminalOf(origin.viewId).dataCb?.("whoami\r");
    await vi.advanceTimersByTimeAsync(20);
    expect(inputsFor(inputSpy, origin.sessionId as string)).toContain("whoami\r");
    expect(inputsFor(inputSpy, sibling.sessionId as string)).toEqual([]);
    controller.stop();
    mock.dispose();
  });

  it("sends the same keystrokes to every pane in the tab when broadcast is on", async () => {
    const { mock, controller, origin, sibling, terminalOf } = await twoLivePanes();
    useWorkbenchStore.getState().toggleBroadcastInput(true);
    const inputSpy = vi.spyOn(mock, "sessionInput");
    terminalOf(origin.viewId).dataCb?.("whoami\r");
    await vi.advanceTimersByTimeAsync(20);
    expect(inputsFor(inputSpy, origin.sessionId as string)).toContain("whoami\r");
    expect(inputsFor(inputSpy, sibling.sessionId as string)).toContain("whoami\r");
    controller.stop();
    mock.dispose();
  });

  it("pastes with CR line endings and brackets only when the target shell enabled the mode (W3 Windows)", async () => {
    const { mock, controller, origin, sibling, terminalOf } = await twoLivePanes();
    useWorkbenchStore.getState().toggleBroadcastInput(true);
    vi.stubGlobal("navigator", { clipboard: { readText: async () => "dir\r\ncd ..\n" } });
    try {
      const inputSpy = vi.spyOn(mock, "sessionInput");
      // 원본(포커스) pane의 셸은 DECSET 2004를 켰고, 형제(cmd.exe)는 모른다.
      terminalOf(origin.viewId).modes.bracketedPasteMode = true;
      terminalOf(sibling.viewId).modes.bracketedPasteMode = false;
      await controller.pasteFromClipboard();
      await vi.advanceTimersByTimeAsync(50);
      expect(inputsFor(inputSpy, origin.sessionId as string)).toContain("\x1b[200~dir\rcd ..\r\x1b[201~");
      expect(inputsFor(inputSpy, sibling.sessionId as string)).toContain("dir\rcd ..\r");

      // 둘 다 꺼져 있으면 아무도 마커를 받지 않는다.
      inputSpy.mockClear();
      terminalOf(origin.viewId).modes.bracketedPasteMode = false;
      await controller.pasteFromClipboard();
      await vi.advanceTimersByTimeAsync(50);
      expect(inputsFor(inputSpy, origin.sessionId as string)).toEqual(["dir\rcd ..\r"]);
      expect(inputsFor(inputSpy, sibling.sessionId as string)).toEqual(["dir\rcd ..\r"]);
    } finally {
      vi.unstubAllGlobals();
      controller.stop();
      mock.dispose();
    }
  });

  it("never broadcasts terminal-generated reports", async () => {
    const { mock, controller, origin, sibling, terminalOf } = await twoLivePanes();
    useWorkbenchStore.getState().toggleBroadcastInput(true);
    const inputSpy = vi.spyOn(mock, "sessionInput");
    // 커서 위치 응답: 원본 세션에는 그대로 가야 하지만 형제에게는 잡음이다.
    terminalOf(origin.viewId).dataCb?.("\x1b[24;80R");
    await vi.advanceTimersByTimeAsync(20);
    expect(inputsFor(inputSpy, origin.sessionId as string)).toContain("\x1b[24;80R");
    expect(inputsFor(inputSpy, sibling.sessionId as string)).toEqual([]);
    controller.stop();
    mock.dispose();
  });

  it("does not reflect broadcast input back into the sending panes", async () => {
    const { mock, controller, origin, sibling, terminalOf } = await twoLivePanes();
    useWorkbenchStore.getState().toggleBroadcastInput(true);
    const inputSpy = vi.spyOn(mock, "sessionInput");
    terminalOf(origin.viewId).dataCb?.("x");
    await vi.advanceTimersByTimeAsync(20);
    // 배분받은 입력이 다시 배분되면 pane 수만큼 반사가 늘어난다.
    expect(inputsFor(inputSpy, origin.sessionId as string)).toEqual(["x"]);
    expect(inputsFor(inputSpy, sibling.sessionId as string)).toEqual(["x"]);
    controller.stop();
    mock.dispose();
  });
});

describe("종료 알림 — 관리 작업만(4차 검토)", () => {
  beforeEach(() => {
    resetStore();
    useNotificationStore.getState().clear();
  });
  afterEach(() => {
    useNotificationStore.getState().clear();
  });

  function paneMetaOf(leafId: string, viewId: string, workloadId: string) {
    return {
      leafId,
      viewId,
      sessionId: `session-${leafId}`,
      workloadId,
      title: `t-${leafId}`,
      cwd: "/repo",
      phase: "live" as const,
      error: null,
      usage: null,
      flowBlocked: false,
    };
  }

  function seedInactiveTabPane(workloadId: string) {
    useWorkbenchStore.setState((s) => ({
      tabs: [
        { kind: "terminal" as const, id: "tabA", title: "A", root: { kind: "leaf" as const, id: "leafA", view_id: "vA", session_id: "sA" } },
        { kind: "terminal" as const, id: "tabB", title: "B", root: { kind: "leaf" as const, id: "leafB", view_id: "vB", session_id: "sB" } },
      ],
      activeTabId: "tabA",
      focusedLeafId: "leafA",
      panes: {
        ...s.panes,
        leafA: paneMetaOf("leafA", "vA", "w-A"),
        leafB: paneMetaOf("leafB", "vB", workloadId),
      },
    }));
  }

  it("비활성 탭의 관리 작업 종료만 알림센터에 남는다 — 셸 종료는 남기지 않는다", () => {
    const { controller } = makeStack();
    seedInactiveTabPane("w-managed");

    const summary = (workloadId: string, mode: "shell" | "managed") =>
      ({
        workload_id: workloadId,
        session_id: `session-leafB`,
        mode,
        state: "FAILED",
        priority: 1,
        title: "agent",
        cwd: "/repo",
        program: "claude",
        reservation_bytes: "2147483648",
        cpu_slots: 1,
        enforcement: "observe",
        root_exited: true,
        cancel_requested: false,
        exit_code: 1,
        queue_reason: null,
        connection: "attached",
        usage: null,
      }) as never;

    controller.handleEvent({ kind: "workload.changed", payload: summary("w-managed", "managed") });
    controller.handleEvent({ kind: "workload.changed", payload: summary("w-shell", "shell") });

    const items = useNotificationStore.getState().items;
    expect(items.some((item) => item.id.startsWith("finished:w-managed"))).toBe(true);
    expect(items.some((item) => item.id.startsWith("finished:w-shell"))).toBe(false);
  });

  it("활성 탭의 작업 종료는 사용자가 보고 있으니 침묵한다", () => {
    const { controller } = makeStack();
    seedInactiveTabPane("w-managed");
    useWorkbenchStore.setState({ activeTabId: "tabB" });
    controller.handleEvent({
      kind: "workload.changed",
      payload: ({
        workload_id: "w-managed",
        session_id: "session-leafB",
        mode: "managed",
        state: "SUCCEEDED",
        priority: 1,
        title: "agent",
        cwd: "/repo",
        program: "claude",
        reservation_bytes: "2147483648",
        cpu_slots: 1,
        enforcement: "observe",
        root_exited: true,
        cancel_requested: false,
        exit_code: 0,
        queue_reason: null,
        connection: "attached",
        usage: null,
      }) as never,
    });
    expect(useNotificationStore.getState().items).toHaveLength(0);
  });

  it("감지된 에이전트가 workload.changed → pane 배지로 내려오고 세션 종료로 지워진다", async () => {
    const { mock, controller, registry } = makeStack();
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];

    const summaryWith = (agent: { agent: string; pid: number; detected_at_ms: number } | null) =>
      ({
        workload_id: pane.workloadId,
        session_id: pane.sessionId,
        mode: "shell",
        state: "RUNNING",
        priority: 1,
        title: "zsh",
        cwd: "/repo",
        program: "/bin/zsh",
        reservation_bytes: "2147483648",
        cpu_slots: 1,
        enforcement: "observe",
        root_exited: false,
        cancel_requested: false,
        exit_code: null,
        queue_reason: null,
        connection: "attached",
        usage: null,
        agent,
      }) as never;

    controller.handleEvent({
      kind: "workload.changed",
      payload: summaryWith({ agent: "claude", pid: 4321, detected_at_ms: 100 }),
    });
    expect(useWorkbenchStore.getState().panes[pane.leafId].agent?.agent).toBe("claude");

    // 같은 감지 결과가 반복돼도 상태는 그대로다.
    controller.handleEvent({
      kind: "workload.changed",
      payload: summaryWith({ agent: "claude", pid: 4321, detected_at_ms: 100 }),
    });
    expect(useWorkbenchStore.getState().panes[pane.leafId].agent?.pid).toBe(4321);

    // 에이전트가 빠져나가면 배지가 내려간다.
    controller.handleEvent({ kind: "workload.changed", payload: summaryWith(null) });
    expect(useWorkbenchStore.getState().panes[pane.leafId].agent).toBeNull();

    // 다시 감지된 뒤 세션이 종료되면 요약 잔상과 무관하게 지워진다.
    controller.handleEvent({
      kind: "workload.changed",
      payload: summaryWith({ agent: "codex", pid: 8642, detected_at_ms: 200 }),
    });
    controller.handleEvent({
      kind: "session.exited",
      payload: { session_id: pane.sessionId, epoch: "e", exit_code: 0, reason: "NORMAL" } as never,
    });
    expect(useWorkbenchStore.getState().panes[pane.leafId].agent).toBeNull();
    expect(useWorkbenchStore.getState().panes[pane.leafId].phase).toBe("exited");
    controller.stop(); registry.disposeAll(); mock.dispose();
  });
});

describe("복원 시 고아 세션 cwd별 묶기(04-ui §2-5)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetStore();
    usePreferences.setState({ terminateOnClose: false });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("groups live sessions that are missing from the saved layout into one tab per cwd, not one tab per session", async () => {
    const { mock, registry, controller, launchSpy } = makeStack();
    controller.start();
    controller.newTerminal(); // cwd = home (C:\Users)
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const first = Object.values(useWorkbenchStore.getState().panes)[0];
    expect(controller.splitFocused("row", "D:\\work\\api")).toBe(true); // 다른 프로젝트
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes).filter(p => p.phase === "live")).toHaveLength(2));
    useWorkbenchStore.getState().focusPane(first.leafId);
    expect(controller.splitFocused("row")).toBe(true); // 첫 pane의 cwd(home)를 잇는다
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes).filter(p => p.phase === "live")).toHaveLength(3));
    const sessionIds = Object.values(useWorkbenchStore.getState().panes).map(p => p.sessionId).sort();
    controller.stop();
    registry.disposeAll();
    resetStore(); // 저장된 배치 없음 → 셋 다 고아

    const restoredRegistry = new TerminalRegistry({ createTerminal: () => new StackTerminal(), createDom: () => ({ className: "", parentElement: null, remove: () => undefined }) });
    const restored = new SessionController({ client: mock, registry: restoredRegistry, platform: "windows" });
    restored.start();
    await restored.restoreWorkspace();
    await vi.advanceTimersByTimeAsync(100);

    const state = useWorkbenchStore.getState();
    expect(launchSpy).toHaveBeenCalledTimes(3); // 복원은 아무것도 새로 띄우지 않는다
    expect(Object.values(state.panes).map(p => p.sessionId).sort()).toEqual(sessionIds);
    const snapshot = await mock.systemSnapshot();
    const apiWorkload = snapshot.workloads.find(w => w.cwd === "D:\\work\\api")!;
    // cwd가 같은 두 세션은 한 탭(이름 = 경로 마지막 조각), 혼자인 세션은 작업 제목 그대로.
    expect(state.tabs.map(tab => [tab.title, leafCount(tab.root)])).toEqual([
      ["Users", 2],
      [apiWorkload.title, 1],
    ]);
    expect(state.tabs[0].root?.kind).toBe("split");
    expect(state.activeTabId).toBe(state.tabs[0].id);
    expect(Object.values(state.panes).every(p => p.phase === "live")).toBe(true);
    restored.stop();
    restoredRegistry.disposeAll();
    mock.dispose();
  });

  it("groupOrphansByCwd: same cwd → one group named after the folder, lone workloads keep their title, cap splits with numbers", () => {
    const w = (id: string, cwd: string, title = `job-${id}`) => ({ workload_id: id, cwd, title, session_id: `s-${id}` }) as unknown as import("../src/generated/WorkloadSummary").WorkloadSummary;
    const groups = groupOrphansByCwd([w("1", "/repo/x"), w("2", "/repo/y"), w("3", "/repo/x"), w("4", "/repo/x")], 2);
    expect(groups.map(g => [g.title, g.workloads.map(x => x.workload_id)])).toEqual([
      ["x 1", ["1", "3"]],
      ["x 2", ["4"]],
      ["job-2", ["2"]],
    ]);
  });
});

describe("attach to an already-attached session", () => {
  it("focuses the existing pane instead of opening a second view that would freeze the first", async () => {
    const { mock, controller, registry } = makeStack();
    controller.start(); controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    controller.attachWorkloadTerminal(pane.workloadId!);
    expect(Object.keys(useWorkbenchStore.getState().panes)).toHaveLength(1);
    expect(useWorkbenchStore.getState().focusedLeafId).toBe(pane.leafId);
    expect(useWorkbenchStore.getState().panes[pane.leafId].phase).toBe("live");
    controller.stop(); registry.disposeAll(); mock.dispose();
  });
});


describe("workload panel shortcut", () => {
  it("opens and closes the right drawer", () => {
    const { mock, controller } = makeStack();
    useWorkbenchStore.setState({ queueDrawerOpen: false });
    controller.dispatchShortcut("queue-toggle");
    expect(useWorkbenchStore.getState().queueDrawerOpen).toBe(true);
    controller.dispatchShortcut("queue-toggle");
    expect(useWorkbenchStore.getState().queueDrawerOpen).toBe(false);
    controller.stop();
    mock.dispose();
  });
});

describe("배치 편집(04-ui §2-6) — 그룹 삭제와 떨어져 나간 터미널 다시 붙이기", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetStore();
    usePreferences.setState({ terminateOnClose: false });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("deleting a group detaches without cancelling, lists the terminal as unplaced, and dragging it back reattaches without relaunching", async () => {
    const { mock, controller, registry, launchSpy, detachSpy } = makeStack();
    const cancel = vi.spyOn(mock, "workloadCancel");
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    const tabId = useWorkbenchStore.getState().activeTabId!;

    await controller.ungroupTab(tabId);
    expect(cancel).not.toHaveBeenCalled();
    expect(detachSpy).toHaveBeenCalledWith(expect.objectContaining({ session_id: pane.sessionId }));
    expect(useWorkbenchStore.getState().tabs).toHaveLength(0);
    expect(useWorkbenchStore.getState().toast).toBe(t("layoutEditor.groupDeleted", { n: 1 }));
    await vi.waitFor(() =>
      expect(layoutRoster(useWorkbenchStore.getState()).unplaced.map((item) => item.sessionId)).toEqual([pane.sessionId]),
    );

    // 새 그룹에 끌어 넣기: 같은 세션을 새 view로 다시 붙이고, 새로 실행하지 않는다.
    expect(
      controller.applyLayoutDrop({
        kind: "attach-session",
        sessionId: pane.sessionId!,
        workloadId: pane.workloadId,
        placement: { kind: "new-tab", atIndex: 0 },
      }),
    ).toBe(true);
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const reattached = Object.values(useWorkbenchStore.getState().panes)[0];
    expect(reattached.sessionId).toBe(pane.sessionId);
    expect(reattached.workloadId).toBe(pane.workloadId);
    expect(reattached.viewId).not.toBe(pane.viewId);
    expect(launchSpy).toHaveBeenCalledOnce();
    expect(useWorkbenchStore.getState().tabs).toHaveLength(1);
    expect(layoutRoster(useWorkbenchStore.getState()).unplaced).toEqual([]);

    // 이미 붙어 있는 세션은 두 번 붙이지 않는다(세션당 화면은 하나).
    expect(controller.attachSessionAt(pane.sessionId!, pane.workloadId, { kind: "new-tab", atIndex: 0 })).toBe(false);
    expect(useWorkbenchStore.getState().tabs).toHaveLength(1);
    controller.stop();
    registry.disposeAll();
    mock.dispose();
  });

  it("the layout-editor shortcut opens the editor and the same shortcut comes back", () => {
    const { mock, controller } = makeStack();
    useWorkbenchStore.setState({ page: "terminal" });
    controller.dispatchShortcut("layout-editor");
    expect(useWorkbenchStore.getState().page).toBe("layout");
    controller.dispatchShortcut("layout-editor");
    expect(useWorkbenchStore.getState().page).toBe("terminal");
    controller.stop();
    mock.dispose();
  });

  it("'+ new group' adds an empty group at the end without switching away from the tab being viewed", () => {
    const { mock, controller } = makeStack();
    useWorkbenchStore.setState({
      tabs: [{ kind: "terminal", id: "t1", title: "api", root: null }],
      activeTabId: "t1",
    } as Partial<WorkbenchState>);
    const tabId = controller.newGroup();
    const state = useWorkbenchStore.getState();
    expect(state.tabs.map((tab) => tab.id)).toEqual(["t1", tabId]);
    expect(state.tabs[1]).toMatchObject({ kind: "terminal", root: null });
    expect(state.activeTabId).toBe("t1");
    controller.stop();
    mock.dispose();
  });

  it("a group being deleted ignores a second ×, refuses drops, and detaches a pane that slips in instead of orphaning it", async () => {
    const { mock, controller, registry, detachSpy } = makeStack();
    const cancel = vi.spyOn(mock, "workloadCancel");
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const groupTab = useWorkbenchStore.getState().activeTabId!;
    const first = Object.values(useWorkbenchStore.getState().panes)[0];
    controller.newTab();
    controller.newTerminal();
    await vi.waitFor(() =>
      expect(Object.values(useWorkbenchStore.getState().panes).filter((pane) => pane.phase === "live")).toHaveLength(2),
    );
    const second = Object.values(useWorkbenchStore.getState().panes).find((pane) => pane.leafId !== first.leafId)!;

    // 첫 detach 왕복이 끝나기 전(await 없이) 같은 탭에서 벌어지는 일들.
    const deleting = controller.ungroupTab(groupTab);
    void controller.ungroupTab(groupTab); // 두 번째 ×는 무시된다
    expect(
      controller.applyLayoutDrop({ kind: "move-pane-to-tab", leafId: second.leafId, targetTabId: groupTab }),
    ).toBe(false);
    // 끌어 놓기가 아닌 경로(메뉴)로 끼어든 창도 스토어에서만 지워지지 않고 창만 닫기로 떼어진다.
    expect(controller.movePaneToTab(second.leafId, groupTab)).toBe(true);
    await deleting;

    const state = useWorkbenchStore.getState();
    expect(state.tabs.some((tab) => tab.id === groupTab)).toBe(false);
    expect(state.panes[first.leafId]).toBeUndefined();
    expect(state.panes[second.leafId]).toBeUndefined();
    expect(detachSpy).toHaveBeenCalledWith(expect.objectContaining({ session_id: first.sessionId }));
    expect(detachSpy).toHaveBeenCalledWith(expect.objectContaining({ session_id: second.sessionId }));
    expect(cancel).not.toHaveBeenCalled();
    expect(state.toast).toBe(t("layoutEditor.groupDeleted", { n: 2 }));
    await vi.waitFor(() =>
      expect(layoutRoster(useWorkbenchStore.getState()).unplaced.map((item) => item.sessionId).sort()).toEqual(
        [first.sessionId, second.sessionId].sort(),
      ),
    );
    // 떼어 낸 세션은 다시 붙일 수 있고(남은 view가 없다), 한 번만 붙는다.
    expect(controller.attachSessionAt(second.sessionId!, second.workloadId, { kind: "new-tab", atIndex: 0 })).toBe(true);
    expect(controller.attachSessionAt(second.sessionId!, second.workloadId, { kind: "new-tab", atIndex: 0 })).toBe(false);
    controller.stop();
    registry.disposeAll();
    mock.dispose();
  });

  it("deleting a group whose terminal is still starting counts it, and the launch lands detached in the unplaced list", async () => {
    const { mock, controller, registry, launchSpy, detachSpy } = makeStack();
    const cancel = vi.spyOn(mock, "workloadCancel");
    const realLaunch = mock.workloadLaunch.bind(mock);
    let land: () => void = () => undefined;
    launchSpy.mockImplementationOnce(
      (request) =>
        new Promise<Awaited<ReturnType<typeof realLaunch>>>((resolve, reject) => {
          land = () => void realLaunch(request).then(resolve, reject);
        }),
    );
    controller.start();
    controller.newTab();
    controller.newTerminal();
    const groupTab = useWorkbenchStore.getState().activeTabId!;
    await vi.waitFor(() => expect(launchSpy).toHaveBeenCalledTimes(1));
    const starting = Object.values(useWorkbenchStore.getState().panes)[0];
    expect(starting.phase).toBe("starting");
    expect(starting.sessionId ?? null).toBeNull();

    await controller.ungroupTab(groupTab);
    expect(useWorkbenchStore.getState().tabs.some((tab) => tab.id === groupTab)).toBe(false);
    expect(useWorkbenchStore.getState().panes[starting.leafId]).toBeUndefined();
    expect(useWorkbenchStore.getState().toast).toBe(t("layoutEditor.groupDeleted", { n: 1 }));

    land();
    // 시작이 끝나면 아무 view 없이 떨어져 나간 채 계속 돈다 — 종료하지 않고 배치 안 됨에 나타난다.
    await vi.waitFor(() => expect(layoutRoster(useWorkbenchStore.getState()).unplaced).toHaveLength(1));
    expect(cancel).not.toHaveBeenCalled();
    expect(detachSpy).not.toHaveBeenCalled();
    controller.stop();
    registry.disposeAll();
    mock.dispose();
  });

  it("a stale session route whose pane already left the store does not block attaching that session again", async () => {
    const { mock, controller, registry, detachSpy } = makeStack();
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    // 컨트롤러를 거치지 않고 스토어에서만 창이 빠진 상태(작업 공간을 통째로 갈아 끼운 경우 등).
    useWorkbenchStore.setState((s) => ({
      tabs: s.tabs.map((tab) => (tab.kind === "terminal" ? { ...tab, root: null } : tab)),
      panes: {},
      focusedLeafId: null,
    }));
    expect(controller.attachSessionAt(pane.sessionId!, pane.workloadId, { kind: "new-tab", atIndex: 0 })).toBe(true);
    // 낡은 view는 데몬에서도 떼어 두 화면이 한 세션을 두고 다투지 않는다.
    expect(detachSpy).toHaveBeenCalledWith({ session_id: pane.sessionId, view_id: pane.viewId });
    await vi.waitFor(() =>
      expect(Object.values(useWorkbenchStore.getState().panes).find((p) => p.sessionId === pane.sessionId)?.phase).toBe("live"),
    );
    controller.stop();
    registry.disposeAll();
    mock.dispose();
  });
});
