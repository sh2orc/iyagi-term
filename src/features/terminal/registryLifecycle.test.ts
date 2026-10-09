import { describe, expect, it, vi } from "vitest";
import {
  TerminalRegistry,
  type MountPoint,
  type RegistryDom,
  type TerminalLike,
} from "./registry";

function fakeTerminal(): TerminalLike {
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
  };
}

describe("TerminalRegistry mount lifecycle", () => {
  it("reattaches an orphaned xterm host after React replaces its pane DOM", () => {
    const dom: RegistryDom = {
      className: "",
      parentElement: null,
      remove: vi.fn(() => {
        dom.parentElement = null;
      }),
    };
    const appendA = vi.fn(() => {
      dom.parentElement = parentA as never;
    });
    const appendB = vi.fn(() => {
      dom.parentElement = parentB as never;
    });
    const parentA: MountPoint = {
      appendChild: appendA,
      getBoundingClientRect: () => ({ width: 800, height: 600 }),
    } as MountPoint;
    const parentB: MountPoint = {
      appendChild: appendB,
      getBoundingClientRect: () => ({ width: 800, height: 600 }),
    } as MountPoint;
    const registry = new TerminalRegistry({
      createTerminal: fakeTerminal,
      createDom: () => dom,
    });
    const entry = registry.acquire("view-1");

    entry.mount(parentA);
    // Simulate WebKit/React removing the old host without the effect cleanup.
    dom.parentElement = null;
    entry.mount(parentB);

    expect(appendA).toHaveBeenCalledTimes(1);
    expect(appendB).toHaveBeenCalledTimes(1);
    expect(dom.remove).toHaveBeenCalledTimes(1);
    expect(dom.parentElement).toBe(parentB);
  });

  it("does not append twice while the same host is still connected", () => {
    const dom: RegistryDom = { className: "", parentElement: null, remove: vi.fn() };
    const append = vi.fn(() => {
      dom.parentElement = parent as never;
    });
    const parent: MountPoint = {
      appendChild: append,
      getBoundingClientRect: () => ({ width: 800, height: 600 }),
    } as MountPoint;
    const registry = new TerminalRegistry({ createTerminal: fakeTerminal, createDom: () => dom });
    const entry = registry.acquire("view-1");

    entry.mount(parent);
    entry.mount(parent);

    expect(append).toHaveBeenCalledTimes(1);
  });
});

describe("TerminalRegistry onOpen cleanup", () => {
  it("runs the onOpen disposer exactly once, before the terminal is disposed", () => {
    const order: string[] = [];
    const registry = new TerminalRegistry({
      createTerminal: () => ({ ...fakeTerminal(), dispose: () => order.push("terminal") }),
      createDom: () => ({ className: "", parentElement: null, remove: vi.fn() }),
      onOpen: () => () => order.push("hook"),
    });
    const entry = registry.acquire("view-1");
    entry.dispose();
    entry.dispose(); // idempotent: the hook must not run again
    expect(order).toEqual(["hook", "terminal", "terminal"]);
  });

  it("tolerates hooks that return nothing or throw", () => {
    const registry = new TerminalRegistry({
      createTerminal: fakeTerminal,
      createDom: () => ({ className: "", parentElement: null, remove: vi.fn() }),
      onOpen: () => undefined,
    });
    expect(() => registry.acquire("v").dispose()).not.toThrow();
    const throwing = new TerminalRegistry({
      createTerminal: fakeTerminal,
      createDom: () => ({ className: "", parentElement: null, remove: vi.fn() }),
      onOpen: () => () => {
        throw new Error("hook failed");
      },
    });
    expect(() => throwing.acquire("v").dispose()).not.toThrow();
    expect(throwing.liveCount).toBe(0);
  });
});
