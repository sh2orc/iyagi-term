import { beforeEach, describe, expect, it } from "vitest";
import type { DaemonClient } from "../daemon/client";
import { SessionController } from "./sessionController";
import { TerminalRegistry, type TerminalLike } from "./registry";
import { DEFAULT_FONT_SIZE, MAX_FONT_SIZE, MIN_FONT_SIZE } from "./zoom";
import { makeLeaf, type SplitNode } from "./splitTree";
import { useWorkbenchStore, type PaneMeta } from "../../store/workbenchStore";

function fakeTerminal(initialFontSize = DEFAULT_FONT_SIZE): TerminalLike {
  return {
    open: () => undefined,
    write: () => undefined,
    resize: () => undefined,
    dispose: () => undefined,
    onData: () => ({ dispose: () => undefined }),
    hasSelection: () => false,
    getSelection: () => "",
    attachCustomKeyEventHandler: () => undefined,
    loadAddon: () => undefined,
    element: null,
    options: { fontSize: initialFontSize },
  } satisfies TerminalLike;
}

function fakeDom(): { className: string; parentElement: null; remove: () => void } {
  return { className: "", parentElement: null, remove: () => undefined };
}

/** viewId들을 registry에 미리 acquire해 둔 controller와 terminal 모음. */
function makeController(viewIds: string[]): {
  controller: SessionController;
  terminals: Map<string, TerminalLike>;
} {
  const registry = new TerminalRegistry({ createTerminal: () => fakeTerminal(), createDom: fakeDom });
  const controller = new SessionController({
    client: {} as unknown as DaemonClient, // zoom 경로는 client를 건드리지 않는다
    registry,
    platform: "windows",
  });
  const terminals = new Map<string, TerminalLike>();
  for (const viewId of viewIds) {
    terminals.set(viewId, registry.acquire(viewId).terminal);
  }
  return { controller, terminals };
}

function pane(viewId: string): PaneMeta {
  return {
    leafId: "l1",
    viewId,
    sessionId: null,
    workloadId: null,
    title: "터미널",
    cwd: null,
    phase: "live",
    error: null,
    usage: null,
    flowBlocked: false,
  };
}

/** leaf들을 좌우 split으로 묶은 root(1개면 leaf 그대로). */
function rootFor(viewIds: Record<string, string>): SplitNode {
  const ids = Object.keys(viewIds);
  let root: SplitNode = makeLeaf(ids[0], viewIds[ids[0]]);
  for (let i = 1; i < ids.length; i++) {
    root = {
      kind: "split",
      id: `s${i}`,
      axis: "row",
      ratio: 0.5,
      first: root,
      second: makeLeaf(ids[i], viewIds[ids[i]]),
    };
  }
  return root;
}

function resetStore(viewIds: Record<string, string>, focused: string | null): void {
  const panes: Record<string, PaneMeta> = {};
  for (const leafId of Object.keys(viewIds)) {
    panes[leafId] = { ...pane(viewIds[leafId]), leafId };
  }
  useWorkbenchStore.setState({
    tabs: [{ kind: "terminal" as const, id: "t1", title: "t1", root: rootFor(viewIds) }],
    activeTabId: "t1",
    focusedLeafId: focused,
    panes,
  });
}

describe("SessionController.zoomFocused / zoom shortcuts", () => {
  beforeEach(() => {
    useWorkbenchStore.setState({
      tabs: [],
      activeTabId: null,
      focusedLeafId: null,
      panes: {},
    });
  });

  it("grows, shrinks, and resets the focused pane's font", () => {
    const { controller, terminals } = makeController(["v1"]);
    resetStore({ l1: "v1" }, "l1");
    const terminal = terminals.get("v1")!;

    controller.dispatchShortcut("zoom-in");
    expect(terminal.options?.fontSize).toBe(DEFAULT_FONT_SIZE + 1);
    controller.dispatchShortcut("zoom-out");
    controller.dispatchShortcut("zoom-out");
    expect(terminal.options?.fontSize).toBe(DEFAULT_FONT_SIZE - 1);
    controller.dispatchShortcut("zoom-reset");
    expect(terminal.options?.fontSize).toBe(DEFAULT_FONT_SIZE);
  });

  it("affects only the focused pane (형제 pane은 그대로)", () => {
    const { controller, terminals } = makeController(["v1", "v2"]);
    resetStore({ l1: "v1", l2: "v2" }, "l2");

    controller.dispatchShortcut("zoom-in");
    controller.dispatchShortcut("zoom-in");

    expect(terminals.get("v1")?.options?.fontSize).toBe(DEFAULT_FONT_SIZE);
    expect(terminals.get("v2")?.options?.fontSize).toBe(DEFAULT_FONT_SIZE + 2);
  });

  it("saturates at the zoom limits instead of throwing", () => {
    const upper = makeController(["v1"]);
    resetStore({ l1: "v1" }, "l1");
    upper.terminals.get("v1")!.options!.fontSize = MAX_FONT_SIZE;
    upper.controller.dispatchShortcut("zoom-in");
    expect(upper.terminals.get("v1")?.options?.fontSize).toBe(MAX_FONT_SIZE);

    const lower = makeController(["v2"]);
    resetStore({ l1: "v2" }, "l1");
    lower.terminals.get("v2")!.options!.fontSize = MIN_FONT_SIZE;
    lower.controller.dispatchShortcut("zoom-out");
    expect(lower.terminals.get("v2")?.options?.fontSize).toBe(MIN_FONT_SIZE);
  });

  it("is a safe no-op without a focused pane or a registry entry", () => {
    const { controller } = makeController([]);
    resetStore({ l1: "v-nope" }, null);
    expect(() => controller.dispatchShortcut("zoom-in")).not.toThrow();

    resetStore({ l1: "v-missing" }, "l1"); // registry에 없는 viewId
    expect(() => controller.dispatchShortcut("zoom-in")).not.toThrow();
  });
});
