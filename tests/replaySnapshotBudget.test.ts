/**
 * 화면 스냅샷 예산(SessionController + MockDaemonClient):
 * - 뜨지 못한 세션은 쉰다 — 어림(버퍼 줄 수 × 열)이 상한을 넘으면 직렬화하지 않고
 *   10분, 직렬화가 던지면 1분. 그동안 주기 점검은 다른 세션에 차례를 준다.
 * - 숨긴 탭·숨은 창 경로도 같은 세션은 60초에 한 번만 뜨고, 창이 숨으면 pane마다
 *   따로(태스크 하나씩) 뜬다. 종료 직전에는 한꺼번에, 최소 간격을 넘어 뜬다.
 * - pane을 닫으면 그 세션의 스냅샷을 지우고, 남은 것은 한 시간마다 치운다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MockDaemonClient } from "../src/features/daemon/mockClient";
import { TerminalRegistry, type RegistryDom, type TerminalLike } from "../src/features/terminal/registry";
import { SessionController } from "../src/features/terminal/sessionController";
import { createMemorySnapshotStore } from "../src/features/terminal/replaySnapshot";
import { useWorkbenchStore, type WorkbenchState } from "../src/store/workbenchStore";
import { usePreferences } from "../src/store/preferences";

class SizedTerminal implements TerminalLike {
  cols = 80;
  rows = 24;
  /** 직렬화 어림이 읽는 버퍼 줄 수(스크롤백 + 화면). */
  bufferLines = 100;
  throws = false;
  open(): void {}
  write(_data: Uint8Array | string, callback?: () => void): void {
    if (callback) queueMicrotask(() => callback());
  }
  resize(): void {}
  dispose(): void {}
  onData(): { dispose(): void } {
    return { dispose: () => undefined };
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
  get buffer() {
    return {
      normal: { length: this.bufferLines },
      active: { type: "normal" as const, baseY: 0, viewportY: 0, cursorY: 0, length: this.bufferLines, getLine: () => undefined },
    };
  }
}

const encode = (text: string) => new TextEncoder().encode(text);

function setup(options: { visibility?: boolean } = {}) {
  let n = 0;
  const mock = new MockDaemonClient({ schedule: (fn) => fn(), resourceIntervalMs: 0, uuid: () => `u${++n}` });
  const terminals: SizedTerminal[] = [];
  const registry = new TerminalRegistry({
    createTerminal: () => {
      const terminal = new SizedTerminal();
      terminals.push(terminal);
      return terminal;
    },
    createDom: () => ({ className: "", parentElement: null, remove: () => undefined }) as RegistryDom,
  });
  const store = createMemorySnapshotStore();
  const serialized: SizedTerminal[] = [];
  const serializeTerminal = (terminal: unknown) => {
    const sized = terminal as SizedTerminal;
    serialized.push(sized);
    if (sized.throws) throw new Error("serialize exploded");
    return { data: "[screen]", cols: 80, rows: 24 };
  };
  const listeners = new Set<() => void>();
  const fakeDocument = {
    hidden: false,
    addEventListener: (_type: string, listener: () => void) => listeners.add(listener),
    removeEventListener: (_type: string, listener: () => void) => listeners.delete(listener),
  };
  if (options.visibility) vi.stubGlobal("document", fakeDocument);
  const controller = new SessionController({
    client: mock,
    registry,
    platform: "darwin",
    replaySnapshots: store,
    serializeTerminal,
  });
  return {
    mock,
    registry,
    controller,
    store,
    terminals,
    serialized,
    setHidden: (hidden: boolean) => {
      fakeDocument.hidden = hidden;
      for (const listener of [...listeners]) listener();
    },
  };
}

/** pane 둘(같은 탭)을 띄우고 각자 출력을 남긴다. */
async function twoLivePanes(h: ReturnType<typeof setup>) {
  h.controller.start();
  h.controller.newTerminal();
  await vi.waitFor(() => expect(Object.values(useWorkbenchStore.getState().panes)[0]?.phase).toBe("live"));
  h.controller.splitFocused("row");
  await vi.waitFor(() =>
    expect(Object.values(useWorkbenchStore.getState().panes).filter((pane) => pane.phase === "live")).toHaveLength(2));
  const panes = Object.values(useWorkbenchStore.getState().panes);
  for (const pane of panes) h.mock.emitProgramOutput(pane.sessionId!, encode(`hello ${pane.leafId}\r\n`));
  await vi.advanceTimersByTimeAsync(100);
  return panes;
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

describe("replay snapshot budget", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resetStore();
    usePreferences.setState({ terminateOnClose: false });
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("어림이 상한을 넘는 화면은 직렬화하지 않고 쉬며, 그동안 다른 세션이 차례를 받는다", async () => {
    const h = setup();
    const [first, second] = await twoLivePanes(h);
    // 첫 pane: 10만 줄 × 200열 = 2천만 셀 > 1,600만 문자 상한.
    h.terminals[0].bufferLines = 100_000;
    h.terminals[0].cols = 200;

    await vi.advanceTimersByTimeAsync(5000);
    expect(h.serialized).toEqual([]);
    expect(h.store.entries.has(first.sessionId!)).toBe(false);
    // 다음 틱은 쉬는 세션을 건너뛰고 둘째를 뜬다.
    await vi.advanceTimersByTimeAsync(5000);
    expect(h.serialized).toEqual([h.terminals[1]]);
    await vi.waitFor(() => expect(h.store.entries.has(second.sessionId!)).toBe(true));

    // 출력이 계속 와도 10분 동안은 다시 시도하지 않는다(틱마다 직렬화해 버리던 것).
    h.mock.emitProgramOutput(first.sessionId!, encode("more\r\n"));
    await vi.advanceTimersByTimeAsync(9 * 60_000);
    expect(h.serialized.filter((terminal) => terminal === h.terminals[0])).toEqual([]);
    h.terminals[0].bufferLines = 1000;
    await vi.advanceTimersByTimeAsync(70_000);
    expect(h.serialized).toContain(h.terminals[0]);
    h.controller.dispose();
    h.mock.dispose();
  });

  it("직렬화가 던지면 1분 쉰다", async () => {
    const h = setup();
    const [first] = await twoLivePanes(h);
    h.terminals[0].throws = true;
    await vi.advanceTimersByTimeAsync(5000);
    expect(h.serialized).toEqual([h.terminals[0]]);
    await vi.advanceTimersByTimeAsync(5000);
    expect(h.serialized).toEqual([h.terminals[0], h.terminals[1]]);
    h.terminals[0].throws = false;
    h.mock.emitProgramOutput(first.sessionId!, encode("again\r\n"));
    await vi.advanceTimersByTimeAsync(40_000);
    expect(h.serialized.filter((terminal) => terminal === h.terminals[0])).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(25_000);
    expect(h.serialized.filter((terminal) => terminal === h.terminals[0])).toHaveLength(2);
    await vi.waitFor(() => expect(h.store.entries.has(first.sessionId!)).toBe(true));
    h.controller.dispose();
    h.mock.dispose();
  });

  it("창이 숨으면 pane마다 따로 뜨고, 다시 숨겨도 60초 안에는 같은 세션을 다시 뜨지 않는다; 종료는 간격을 넘는다", async () => {
    const h = setup({ visibility: true });
    const [first, second] = await twoLivePanes(h);

    h.setHidden(true);
    // 첫 태스크 전에는 아무것도 뜨지 않고, 태스크마다 하나씩 뜬다.
    expect(h.serialized).toEqual([]);
    await vi.advanceTimersByTimeAsync(0);
    expect(h.serialized.length).toBeGreaterThanOrEqual(1);
    await vi.advanceTimersByTimeAsync(10);
    expect(h.serialized).toEqual([h.terminals[0], h.terminals[1]]);
    // 숨은 동안에는 주기 점검도 돌지 않는다.
    h.mock.emitProgramOutput(first.sessionId!, encode("while hidden\r\n"));
    await vi.advanceTimersByTimeAsync(20_000);
    expect(h.serialized).toHaveLength(2);

    // 다시 보였다 숨어도 60초가 지나지 않았으면 다시 뜨지 않는다.
    h.setHidden(false);
    h.setHidden(true);
    await vi.advanceTimersByTimeAsync(100);
    expect(h.serialized).toHaveLength(2);

    // 종료 직전은 마지막 기회 — 간격을 넘어 바뀐 화면을 뜬다.
    h.mock.emitProgramOutput(second.sessionId!, encode("before quit\r\n"));
    await vi.advanceTimersByTimeAsync(100);
    await h.controller.confirmQuit(false);
    expect(h.serialized.slice(2)).toEqual([h.terminals[0], h.terminals[1]]);
    h.controller.dispose();
    h.mock.dispose();
  });

  it("pane을 닫으면 그 세션의 스냅샷을 지우고, 남은 것은 한 시간마다 치운다", async () => {
    const h = setup();
    const [first, second] = await twoLivePanes(h);
    await vi.advanceTimersByTimeAsync(10_000);
    await vi.waitFor(() => expect(h.store.entries.size).toBe(2));

    await h.controller.closePane(first.leafId, false);
    await vi.advanceTimersByTimeAsync(0);
    expect(h.store.entries.has(first.sessionId!)).toBe(false);
    expect(h.store.entries.has(second.sessionId!)).toBe(true);

    // 창만 닫기·비정상 종료로 남은 스냅샷(여기서는 직접 심는다)은 주기 정리가 치운다.
    await h.store.save({ ...h.store.entries.get(second.sessionId!)!, sessionId: "orphan" });
    expect(h.store.entries.has("orphan")).toBe(true);
    await vi.advanceTimersByTimeAsync(60 * 60_000);
    expect(h.store.entries.has("orphan")).toBe(false);
    expect(h.store.entries.has(second.sessionId!)).toBe(true);
    h.controller.dispose();
    h.mock.dispose();
  });
});
