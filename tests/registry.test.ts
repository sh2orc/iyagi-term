import { describe, expect, it, vi } from "vitest";
import {
  TerminalRegistry,
  runStrictModeEffect,
  type FitAddonLike,
  type MountPoint,
  type RegistryDom,
  type TerminalLike,
} from "../src/features/terminal/registry";

class FakeTerminal implements TerminalLike {
  openedWith: unknown[] = [];
  disposed = false;
  addons: unknown[] = [];
  writeCalls = 0;

  open(element: unknown): void {
    this.openedWith.push(element);
  }
  write(): void {
    this.writeCalls++;
  }
  resize(): void {
    // no-op
  }
  dispose(): void {
    this.disposed = true;
  }
  onData(): { dispose(): void } {
    return { dispose: () => undefined };
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
  loadAddon(addon: unknown): void {
    this.addons.push(addon);
  }
  element: unknown = null;
}

function makeDom(): RegistryDom {
  return {
    className: "",
    parentElement: null,
    remove: vi.fn(),
  };
}

function makeMount(): MountPoint & { children: unknown[] } {
  const children: unknown[] = [];
  return { children, appendChild: (child: unknown) => children.push(child) };
}

function makeRegistry() {
  const terminals: FakeTerminal[] = [];
  const doms: RegistryDom[] = [];
  const fitFactories: Array<FitAddonLike> = [];
  const observe = vi.fn();
  const disconnect = vi.fn();
  const registry = new TerminalRegistry({
    createTerminal: () => {
      const term = new FakeTerminal();
      terminals.push(term);
      return term;
    },
    createFitAddon: () => {
      const fit: FitAddonLike = { proposeDimensions: () => ({ cols: 97, rows: 31 }) };
      fitFactories.push(fit);
      return fit;
    },
    createResizeObserver: (cb) => ({
      observe: (t) => observe(t, cb),
      unobserve: () => undefined,
      disconnect,
    }),
    createDom: () => {
      const dom = makeDom();
      doms.push(dom);
      return dom;
    },
  });
  return { registry, terminals, doms, observe, disconnect };
}

describe("TerminalRegistry — U09 lifecycle", () => {
  it("acquire is idempotent (one terminal per view)", () => {
    const { registry, terminals } = makeRegistry();
    const a = registry.acquire("view-1");
    const b = registry.acquire("view-1");
    expect(terminals.length).toBe(1);
    expect(a).toBe(b);
    expect(a.terminal.openedWith.length).toBe(1); // open() called exactly once
  });

  it("StrictMode double-mount produces exactly ONE terminal init (effect runner)", () => {
    const { registry, terminals } = makeRegistry();
    const mount = makeMount();
    const cleanup = runStrictModeEffect(() => {
      const entry = registry.acquire("view-1");
      entry.mount(mount);
      return () => entry.unmount();
    });
    // mount → unmount → mount: one terminal, one open, still alive.
    expect(terminals.length).toBe(1);
    expect(terminals[0].openedWith.length).toBe(1);
    expect(terminals[0].disposed).toBe(false);
    expect(mount.children.length).toBe(2); // appended twice (remounted)
    expect(() => cleanup()).not.toThrow();
  });

  it("unmount removes DOM and observer only — never disposes the terminal", () => {
    const { registry, doms, observe, disconnect } = makeRegistry();
    const entry = registry.acquire("view-1");
    const mount = makeMount();
    entry.mount(mount);
    expect(observe).toHaveBeenCalledOnce();
    expect(doms[0].className).toContain("xterm-host");
    entry.unmount();
    expect(disconnect).toHaveBeenCalledOnce();
    expect(doms[0].remove).toHaveBeenCalledOnce();
    expect(entry.terminal.disposed).toBe(false);
    expect(registry.liveCount).toBe(1);
  });

  it("mount/unmount performs no session operations (registry has no client surface)", () => {
    const { registry } = makeRegistry();
    const entry = registry.acquire("view-1");
    const mount = makeMount();
    entry.mount(mount);
    entry.unmount();
    // 구조적 보증: 세션 API가 존재하지 않는다.
    const registryAny = registry as unknown as Record<string, unknown>;
    for (const method of ["workloadLaunch", "sessionAttach", "sessionDetach", "sessionInput", "sessionAck"]) {
      expect(registryAny[method]).toBeUndefined();
    }
  });

  it("explicit disposeView disposes the terminal and drops the entry", () => {
    const { registry, terminals } = makeRegistry();
    const entry = registry.acquire("view-1");
    entry.dispose();
    expect(terminals[0].disposed).toBe(true);
    expect(registry.has("view-1")).toBe(false);
    expect(registry.liveCount).toBe(0);
  });

  it("wires fit results to the handler on mount", () => {
    const { registry } = makeRegistry();
    const dims: unknown[] = [];
    const entry = registry.acquire("view-1");
    registry.setFitHandler("view-1", (d) => dims.push(d));
    const mount = makeMount();
    entry.mount(mount);
    // dom.parentElement is null → px fallback, cols/rows from fake fit addon.
    expect(dims.length).toBe(1);
    expect(dims[0]).toMatchObject({ cols: 97, rows: 31 });
    registry.setFitHandler("view-1", null);
    entry.refit();
    expect(dims.length).toBe(1);
  });

  it("preserves the PTY size while the settings page hides its host", () => {
    const { registry, doms } = makeRegistry();
    const entry = registry.acquire("settings-view");
    let rect = { width: 800, height: 500 };
    doms[0].parentElement = { getBoundingClientRect: () => rect };
    const fit = vi.fn();
    entry.setFitHandler(fit);
    entry.mount(makeMount());
    expect(fit).toHaveBeenCalledTimes(1);
    rect = { width: 0, height: 0 };
    entry.refit();
    expect(fit).toHaveBeenCalledTimes(1);
    rect = { width: 900, height: 600 };
    entry.refit();
    expect(fit).toHaveBeenLastCalledWith(expect.objectContaining(rect));
    expect(fit).toHaveBeenCalledTimes(2);
  });
});

describe("applyGlobalPreferences — 줌 보존(2차 검토)", () => {
  function withZoomedTerminal(zoomed: number) {
    const harness = makeRegistry();
    const entry = harness.registry.acquire("view-zoom");
    (entry.terminal as FakeTerminal).options = { fontSize: zoomed };
    return { registry: harness.registry, terminal: entry.terminal as FakeTerminal };
  }

  it("테마만 바뀌면 pane 글자 크기(줌)를 리셋하지 않는다", () => {
    const { registry, terminal } = withZoomedTerminal(18);
    registry.applyGlobalPreferences({ theme: { background: "#000" } });
    expect(terminal.options?.fontSize).toBe(18); // 줌 유지

    // 기본값 자체가 바뀐 경우에만 새 기본으로 맞춘다.
    registry.applyGlobalPreferences({ theme: { background: "#000" }, fontSize: 15 });
    expect(terminal.options?.fontSize).toBe(15);
  });

  it("글꼴 패밀리 변경은 크기 숫자를 바꾸지 않는다", () => {
    const { registry, terminal } = withZoomedTerminal(20);
    registry.applyGlobalPreferences({ theme: {}, fontFamily: "Menlo, monospace" });
    expect(terminal.options?.fontSize).toBe(20);
    expect(terminal.options?.fontFamily).toBe("Menlo, monospace");
  });
});

describe("renderer lifecycle — 숨은 pane도 렌더러를 살려 재부착을 매끄럽게(LRU 상한)", () => {
  function rendererRegistry(initialEnabled?: boolean) {
    const created: Array<{ dispose: ReturnType<typeof vi.fn> }> = [];
    const registry = new TerminalRegistry({
      createTerminal: () => new FakeTerminal(),
      createDom: makeDom,
      createRenderer: () => {
        const renderer = { dispose: vi.fn() };
        created.push(renderer);
        return renderer;
      },
      ...(initialEnabled === undefined ? {} : { rendererEnabled: initialEnabled }),
    });
    return { registry, created };
  }

  it("mount에서 하나 만들고, 숨겨도(unmount) 살려 두어 재부착 때 재생성하지 않는다", () => {
    const { registry, created } = rendererRegistry();
    const entry = registry.acquire("v1");
    const host = makeMount();
    entry.mount(host);
    expect(created).toHaveLength(1);
    entry.mount(host); // 같은 host 재호출은 새로 만들지 않는다
    expect(created).toHaveLength(1);
    entry.unmount();
    // 숨겨도 렌더러를 폐기하지 않는다 — 재부착이 WebGL 재생성 없이 즉시.
    expect(created[0].dispose).not.toHaveBeenCalled();
    entry.mount(makeMount()); // 다시 보여도 그대로 쓴다(새로 만들지 않는다)
    expect(created).toHaveLength(1);
    // 명시적 dispose(닫기)에서만 회수한다.
    entry.dispose();
    expect(created[0].dispose).toHaveBeenCalledOnce();
  });

  it("설정 토글은 보이는 pane에 즉시 반영되고, 꺼진 채 시작하면 만들지 않는다", () => {
    const { registry, created } = rendererRegistry(false);
    const entry = registry.acquire("v1");
    entry.mount(makeMount());
    expect(created).toHaveLength(0);
    registry.setRendererEnabled(true);
    expect(created).toHaveLength(1);
    registry.setRendererEnabled(false);
    expect(created[0].dispose).toHaveBeenCalledOnce();
    // 숨은(unmount) pane은 켜도 만들지 않는다.
    entry.unmount();
    registry.setRendererEnabled(true);
    expect(created).toHaveLength(1);
  });

  it("렌더러 생성 실패는 mount를 막지 않는다(DOM 렌더러로 물러난다)", () => {
    const registry = new TerminalRegistry({
      createTerminal: () => new FakeTerminal(),
      createDom: makeDom,
      createRenderer: () => {
        throw new Error("WebGL2 not supported");
      },
    });
    const entry = registry.acquire("v1");
    expect(() => entry.mount(makeMount())).not.toThrow();
    expect(() => entry.unmount()).not.toThrow();
  });
});
