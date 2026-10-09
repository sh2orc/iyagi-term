/**
 * 렌더러 유지(LRU) 계약 — 탭 전환을 매끄럽게 하는 핵심.
 *
 * 숨겨도(unmount) WebGL 렌더러를 살려 두고 재부착 때 캐시와 화면을 갱신한다.
 * 다만 브라우저의 WebGL 컨텍스트 예산을 넘기지 않도록 상한을
 * 두고, 초과분은 가장 오래된 "숨은" pane의 렌더러부터만 회수한다(보이는 pane은
 * 절대 건드리지 않는다).
 */

import { describe, expect, it, vi } from "vitest";
import {
  TerminalRegistry,
  type MountPoint,
  type RegistryDom,
  type RendererFactory,
  type RendererHooks,
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

/** dispose를 세는 렌더러 팩토리. */
function trackingRenderers() {
  let created = 0;
  const disposedIds: number[] = [];
  const factory = () => {
    const id = created;
    created += 1;
    return { dispose: () => disposedIds.push(id) };
  };
  return { factory, disposedIds, createdCount: () => created };
}

describe("TerminalRegistry 렌더러 유지(LRU)", () => {
  it("같은 크기로 복귀하거나 부모가 바뀌어도 유지한 GPU 캐시와 전체 화면을 갱신한다", () => {
    const terminal = { ...fakeTerminal(), rows: 24, refresh: vi.fn() };
    const refreshRenderer = vi.fn();
    const createRenderer = vi.fn(() => ({ dispose: vi.fn(), refresh: refreshRenderer }));
    const registry = new TerminalRegistry({
      createTerminal: () => terminal,
      createDom: makeDom,
      createRenderer,
    });
    const entry = registry.acquire("v1");
    const parent = makeParent();
    entry.mount(parent);
    expect(refreshRenderer).not.toHaveBeenCalled();
    terminal.refresh.mockClear();

    entry.unmount();
    entry.mount(parent);
    expect(refreshRenderer).toHaveBeenCalledTimes(1);
    expect(terminal.refresh).toHaveBeenLastCalledWith(0, 23);

    // 단순 refit은 캐시를 버리지 않는다.
    entry.mount(parent);
    expect(refreshRenderer).toHaveBeenCalledTimes(1);
    expect(terminal.refresh).toHaveBeenCalledTimes(1);

    // cleanup 없이 다른 host로 옮기는 경로도 같은 복구가 필요하다.
    entry.mount(makeParent());
    expect(refreshRenderer).toHaveBeenCalledTimes(2);
    expect(terminal.refresh).toHaveBeenCalledTimes(2);
    expect(createRenderer).toHaveBeenCalledTimes(1);
    registry.disposeAll();
  });

  it("숨겨도 렌더러를 폐기하지 않고, 재부착 때 재생성하지 않는다", () => {
    const renderers = trackingRenderers();
    const registry = new TerminalRegistry({
      createTerminal: fakeTerminal,
      createDom: makeDom,
      createRenderer: renderers.factory,
    });
    const entry = registry.acquire("v1");
    entry.mount(makeParent());
    expect(renderers.createdCount()).toBe(1);
    expect(registry.liveRendererCount).toBe(1);

    entry.unmount();
    // 숨겨도 살아 있다 — 폐기되지 않았다.
    expect(renderers.disposedIds).toEqual([]);
    expect(registry.liveRendererCount).toBe(1);

    entry.mount(makeParent());
    // 재부착: 새 렌더러를 만들지 않고 그대로 쓴다.
    expect(renderers.createdCount()).toBe(1);
    expect(registry.liveRendererCount).toBe(1);
  });

  it("dispose는 렌더러를 회수하고, 렌더러 끄기는 유지분까지 해제한다", () => {
    const renderers = trackingRenderers();
    const registry = new TerminalRegistry({
      createTerminal: fakeTerminal,
      createDom: makeDom,
      createRenderer: renderers.factory,
    });
    const a = registry.acquire("a");
    a.mount(makeParent());
    a.unmount(); // 숨김(렌더러 유지)
    expect(registry.liveRendererCount).toBe(1);
    a.dispose();
    expect(renderers.disposedIds.length).toBe(1);
    expect(registry.liveRendererCount).toBe(0);

    // 렌더러 끄기: 숨어서 유지 중이던 것도 즉시 해제한다.
    const b = registry.acquire("b");
    b.mount(makeParent());
    b.unmount();
    expect(registry.liveRendererCount).toBe(1);
    registry.setRendererEnabled(false);
    expect(registry.liveRendererCount).toBe(0);
  });

  it("상한을 넘으면 가장 오래된 숨은 pane부터 회수하고, 보이는 pane은 지킨다", () => {
    const renderers = trackingRenderers();
    const registry = new TerminalRegistry({
      createTerminal: fakeTerminal,
      createDom: makeDom,
      createRenderer: renderers.factory,
    });

    // 보이는(계속 mount된) pane 8개 — 렌더러 id 0..7. 절대 회수되면 안 된다.
    for (let i = 0; i < 8; i += 1) {
      registry.acquire(`vis-${i}`).mount(makeParent());
    }
    expect(registry.liveRendererCount).toBe(8);

    // 숨은 pane을 계속 만들어(렌더러 id 8..15) 상한(12)을 넘긴다.
    for (let i = 0; i < 8; i += 1) {
      const entry = registry.acquire(`hid-${i}`);
      entry.mount(makeParent());
      entry.unmount(); // 숨김
    }

    // 살아 있는 렌더러는 정확히 상한만큼(12): 보이는 8 + 가장 최근 숨은 4.
    expect(registry.liveRendererCount).toBe(12);
    // 회수된 것은 가장 오래된 숨은 pane 4개(id 8..11)뿐 — 보이는 8개(id 0..7)는
    // 하나도 폐기되지 않았다.
    expect(renderers.disposedIds).toEqual([8, 9, 10, 11]);
  });
});

describe("TerminalRegistry 렌더러 컨텍스트 상실(WebGL onDead)", () => {
  /**
   * 컨텍스트를 잃은 WebGL 애드온은 스스로 폐기되며 hooks.onDead를 부른다.
   * 래퍼가 살아 있는 채로 남으면 syncRenderer의 `if (this.renderer) return`에
   * 걸려 그 pane은 영영 DOM 렌더러에 갇히고 LRU 예산도 잘못 센다.
   */
  function contextLossHarness() {
    let created = 0;
    const disposedIds: number[] = [];
    let hooks: RendererHooks | null = null;
    const factory: RendererFactory = (_terminal, h) => {
      hooks = h;
      const id = created;
      created += 1;
      return { dispose: () => disposedIds.push(id) };
    };
    const registry = new TerminalRegistry({
      createTerminal: fakeTerminal,
      createDom: makeDom,
      createRenderer: factory,
    });
    return {
      registry,
      disposedIds,
      createdCount: () => created,
      /** 애드온이 onContextLoss에서 dispose+onDead를 치는 순간을 흉내 낸다. */
      loseContext: () => hooks!.onDead?.(),
    };
  }

  it("죽음을 알리면 더 살아 있다고 세지 않는다(hasLiveRenderer/LRU 계정)", () => {
    const h = contextLossHarness();
    const entry = h.registry.acquire("v1");
    entry.mount(makeParent());
    expect(h.registry.liveRendererCount).toBe(1);

    h.loseContext();
    expect(h.registry.liveRendererCount).toBe(0);
  });

  it("죽어도 visible 상태에서 즉시 재생성하지 않고, 다음 mount가 다시 만든다", () => {
    const h = contextLossHarness();
    const entry = h.registry.acquire("v1");
    entry.mount(makeParent());
    expect(h.createdCount()).toBe(1);

    // 컨텍스트 상실 — DOM 렌더러로 물러난 화면을 유지한 채 재생성 루프를 돌지
    // 않는다(syncRenderer는 mount/설정 변경 때만 온다).
    h.loseContext();
    expect(h.createdCount()).toBe(1);

    // 탭 복귀(unmount → mount): 래퍼가 비워졌으므로 팩토리가 다시 불린다.
    entry.unmount();
    entry.mount(makeParent());
    expect(h.createdCount()).toBe(2);
    expect(h.registry.liveRendererCount).toBe(1);
  });
});
