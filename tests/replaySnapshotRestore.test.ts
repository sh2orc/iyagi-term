/**
 * 앱 재시작 복원 단축(SessionController + MockDaemonClient):
 * - 터미널을 남겨 두고 끝내면 화면 스냅샷을 저장하고, 다음 시작은 스냅샷을 먼저 쓴 뒤
 *   그 뒤에 쌓인 출력만 재생한다(`resume_from_seq`).
 * - 시작할 때 보고 있던 탭의 pane을 먼저 붙이고 나머지는 뒤에 붙인다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MockDaemonClient } from "../src/features/daemon/mockClient";
import { TerminalRegistry, type RegistryDom, type TerminalLike } from "../src/features/terminal/registry";
import { SessionController, type ControllerDeps } from "../src/features/terminal/sessionController";
import { createMemorySnapshotStore, decodeReplaySnapshot } from "../src/features/terminal/replaySnapshot";
import { encodeWorkspace, decodeWorkspace } from "../src/store/workspaceStorage";
import { useWorkbenchStore, type WorkbenchState } from "../src/store/workbenchStore";
import { usePreferences } from "../src/store/preferences";

class StackTerminal implements TerminalLike {
  writes: Uint8Array[] = [];
  dataCb: ((data: string) => void) | null = null;
  open(): void {}
  write(data: Uint8Array | string, callback?: () => void): void {
    this.writes.push(typeof data === "string" ? new TextEncoder().encode(data) : data);
    if (callback) queueMicrotask(() => callback());
  }
  resize(): void {}
  dispose(): void {}
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
  attachCustomKeyEventHandler(): void {}
  loadAddon(): void {}
  element: unknown = null;
  modes = { bracketedPasteMode: false };
  text(): string {
    return this.writes.map((bytes) => new TextDecoder().decode(bytes)).join("");
  }
}

function makeRegistry(terminals: StackTerminal[]): TerminalRegistry {
  return new TerminalRegistry({
    createTerminal: () => {
      const term = new StackTerminal();
      terminals.push(term);
      return term;
    },
    createDom: () => ({ className: "", parentElement: null, remove: () => undefined }) as RegistryDom,
  });
}

function makeMock(): MockDaemonClient {
  let n = 0;
  return new MockDaemonClient({ schedule: (fn) => fn(), resourceIntervalMs: 0, uuid: () => `u${++n}` });
}

function resetStore(): void {
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
    modal: null,
    toast: null,
  } as Partial<WorkbenchState>);
}

/** 저장된 배치로 앱을 다시 켠다(이전 컨트롤러는 프런트만 정리 — PTY는 산다). */
function reload(controller: SessionController, extra: Partial<ControllerDeps>, mock: MockDaemonClient) {
  const saved = encodeWorkspace(useWorkbenchStore.getState());
  controller.dispose();
  resetStore();
  useWorkbenchStore.setState(decodeWorkspace(saved)!);
  const terminals: StackTerminal[] = [];
  const restored = new SessionController({ client: mock, registry: makeRegistry(terminals), platform: "windows", ...extra });
  return { restored, terminals };
}

const encode = (text: string) => new TextEncoder().encode(text);

describe("replay snapshot restore", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetStore();
    usePreferences.setState({ terminateOnClose: false });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("writes the saved screen and replays only the output that followed it", async () => {
    const mock = makeMock();
    const store = createMemorySnapshotStore();
    const serializeTerminal = () => ({ data: "[saved screen]", cols: 80, rows: 24 });
    const terminals: StackTerminal[] = [];
    const controller = new SessionController({
      client: mock, registry: makeRegistry(terminals), platform: "windows", replaySnapshots: store, serializeTerminal,
    });
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    mock.emitProgramOutput(pane.sessionId!, encode("before the snapshot\r\n"));
    await vi.waitFor(() => expect(terminals[0].text()).toContain("before the snapshot"));

    // 터미널을 남겨 두고 끝낸다 → 화면을 저장한다.
    await controller.confirmQuit(false);
    const saved = store.entries.get(pane.sessionId!);
    expect(saved?.data).toBe("[saved screen]");

    mock.emitProgramOutput(pane.sessionId!, encode("after the snapshot\r\n"));
    const attachSpy = vi.spyOn(mock, "sessionAttach");
    const { restored, terminals: restoredTerminals } = reload(controller, { replaySnapshots: store, serializeTerminal }, mock);
    restored.start();
    await restored.restoreWorkspace();
    await vi.waitFor(() => expect(useWorkbenchStore.getState().panes[pane.leafId]?.phase).toBe("live"));

    expect(attachSpy.mock.calls[0][0].resume_from_seq).toBe(String(saved!.seq + 1));
    const screen = restoredTerminals[0].text();
    expect(screen.startsWith("[saved screen]")).toBe(true);
    expect(screen).toContain("after the snapshot");
    expect(screen).not.toContain("before the snapshot");
    restored.dispose();
    mock.dispose();
  });

  it("replays the whole journal when no snapshot was saved", async () => {
    const mock = makeMock();
    const store = createMemorySnapshotStore();
    const serializeTerminal = () => ({ data: "[saved screen]", cols: 80, rows: 24 });
    const controller = new SessionController({ client: mock, registry: makeRegistry([]), platform: "windows" });
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const pane = Object.values(useWorkbenchStore.getState().panes)[0];
    mock.emitProgramOutput(pane.sessionId!, encode("only in the journal\r\n"));

    const attachSpy = vi.spyOn(mock, "sessionAttach");
    const { restored, terminals } = reload(controller, { replaySnapshots: store, serializeTerminal }, mock);
    restored.start();
    await restored.restoreWorkspace();
    await vi.waitFor(() => expect(useWorkbenchStore.getState().panes[pane.leafId]?.phase).toBe("live"));
    expect(attachSpy.mock.calls[0][0].resume_from_seq).toBeUndefined();
    expect(terminals[0].text()).toContain("only in the journal");
    restored.dispose();
    mock.dispose();
  });

  it("attaches the visible tab's panes before the panes of other tabs", async () => {
    const mock = makeMock();
    const controller = new SessionController({ client: mock, registry: makeRegistry([]), platform: "windows" });
    controller.start();
    controller.newTerminal();
    await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
    const first = Object.values(useWorkbenchStore.getState().panes)[0];
    controller.newTab();
    controller.newTerminal();
    await vi.waitFor(() =>
      expect(Object.values(useWorkbenchStore.getState().panes).filter((p) => p.phase === "live")).toHaveLength(2));
    // 두 번째 탭을 보고 있다 — 저장 순서로는 첫 pane이 앞이다.
    const visible = Object.values(useWorkbenchStore.getState().panes).find((p) => p.leafId !== first.leafId)!;

    const attachSpy = vi.spyOn(mock, "sessionAttach");
    const { restored } = reload(controller, {}, mock);
    restored.start();
    await restored.restoreWorkspace();
    expect(attachSpy.mock.calls[0][0].session_id).toBe(visible.sessionId);
    await vi.waitFor(() =>
      expect(Object.values(useWorkbenchStore.getState().panes).every((p) => p.phase === "live")).toBe(true));
    expect(attachSpy.mock.calls.map((call) => call[0].session_id)).toEqual([visible.sessionId, first.sessionId]);
    restored.dispose();
    mock.dispose();
  });

  it("decodes only well-formed snapshots of the requested session", () => {
    const good = {
      sessionId: "s1", seq: 7, cols: 80, rows: 24, data: "x", clearMark: 0,
      terminalTitle: null, historyRebuilder: false, savedAt: 1,
    };
    expect(decodeReplaySnapshot(good, "s1")?.seq).toBe(7);
    expect(decodeReplaySnapshot(good, "s2")).toBeNull();
    expect(decodeReplaySnapshot({ ...good, seq: 0 }, "s1")).toBeNull();
    expect(decodeReplaySnapshot({ ...good, cols: 1.5 }, "s1")).toBeNull();
    expect(decodeReplaySnapshot({ ...good, data: 42 }, "s1")).toBeNull();
    expect(decodeReplaySnapshot({ ...good, clearMark: -3 }, "s1")?.clearMark).toBe(0);
    expect(decodeReplaySnapshot(null, "s1")).toBeNull();
  });
});
