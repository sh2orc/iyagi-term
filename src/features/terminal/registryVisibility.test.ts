/**
 * 표시/숨김 통보 seam — controller가 숨은 view의 pipeline 표시 빈도를 낮추는
 * 데 쓴다(04-ui §4). mount → (viewId, true), unmount → (viewId, false)로
 * 부르며, WebGL 렌더러 유지(LRU)와는 독립이다(그 계약은 registryRenderer.test).
 */

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

function makeDom(): RegistryDom {
  const dom: RegistryDom = {
    className: "",
    parentElement: null,
    remove: vi.fn(() => {
      dom.parentElement = null;
    }),
  };
  return dom;
}

function makeParent(): MountPoint {
  const parent = {
    appendChild: vi.fn((node: RegistryDom) => {
      node.parentElement = parent as never;
    }),
    getBoundingClientRect: () => ({ width: 800, height: 600 }),
  };
  return parent as unknown as MountPoint;
}

describe("TerminalRegistry 표시/숨김 통보", () => {
  it("mount는 visible=true, unmount는 visible=false로 해당 viewId를 알린다", () => {
    const registry = new TerminalRegistry({ createTerminal: fakeTerminal, createDom: makeDom });
    const events: Array<[string, boolean]> = [];
    registry.setVisibilityListener((viewId, visible) => events.push([viewId, visible]));

    const entry = registry.acquire("v1");
    entry.mount(makeParent());
    expect(events).toEqual([["v1", true]]);

    entry.unmount();
    expect(events).toEqual([["v1", true], ["v1", false]]);
  });

  it("리스너를 걸어도 LRU/렌더러 유지 동작은 그대로다", () => {
    let created = 0;
    const disposed: number[] = [];
    const registry = new TerminalRegistry({
      createTerminal: fakeTerminal,
      createDom: makeDom,
      createRenderer: () => {
        const id = created++;
        return { dispose: () => disposed.push(id) };
      },
    });
    const events: Array<[string, boolean]> = [];
    registry.setVisibilityListener((viewId, visible) => events.push([viewId, visible]));

    const entry = registry.acquire("v1");
    entry.mount(makeParent());
    expect(registry.liveRendererCount).toBe(1);

    // 숨겨도 렌더러는 살아 있다(재부착을 매끄럽게) — 표시 통보만 별개로 온다.
    entry.unmount();
    expect(disposed).toEqual([]);
    expect(registry.liveRendererCount).toBe(1);
    expect(events).toEqual([["v1", true], ["v1", false]]);
  });

  it("리스너 미설정/해제 시 mount·unmount는 조용히(no-op) 넘어간다", () => {
    const registry = new TerminalRegistry({ createTerminal: fakeTerminal, createDom: makeDom });
    const entry = registry.acquire("v1");
    // 리스너가 없어도 던지지 않는다.
    expect(() => {
      entry.mount(makeParent());
      entry.unmount();
    }).not.toThrow();

    const events: Array<[string, boolean]> = [];
    registry.setVisibilityListener((viewId, visible) => events.push([viewId, visible]));
    entry.mount(makeParent());
    expect(events).toEqual([["v1", true]]);

    // 해제하면 이후 통보가 멈춘다.
    registry.setVisibilityListener(null);
    entry.unmount();
    expect(events).toEqual([["v1", true]]);
  });
});
