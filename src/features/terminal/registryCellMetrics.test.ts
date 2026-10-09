/**
 * 셀 메트릭 변화 → refit 계약.
 *
 * WebGL 렌더러는 셀 폭을 기기 픽셀로 내리고(Math.floor) DOM 렌더러는 내리지
 * 않는다. 컨텍스트 상실로 DOM 렌더러로 떨어지거나 DPR이 바뀌면 컨테이너
 * 크기는 그대로인데 셀만 넓어진다 — ResizeObserver는 부르지 않으므로
 * registry가 렌더 뒤 셀 크기를 비교해 다시 fit해야 한다. 안 그러면 옛 cols ×
 * 넓어진 셀이 pane을 넘어 긴 입력이 스크롤바 밖으로 그려진다.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { TerminalRegistry, type MountPoint, type RegistryDom, type TerminalLike } from "./registry";

interface CellTerminal extends TerminalLike {
  _core: { _renderService: { dimensions: { css: { cell: { width: number; height: number } } } } };
  emitRender(): void;
  renderListeners: Set<() => void>;
}

function cellTerminal(width: number, height = 15.4): CellTerminal {
  const renderListeners = new Set<() => void>();
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
    onRender: (callback: () => void) => {
      renderListeners.add(callback);
      return { dispose: () => renderListeners.delete(callback) };
    },
    renderListeners,
    emitRender: () => {
      for (const listener of [...renderListeners]) listener();
    },
    _core: { _renderService: { dimensions: { css: { cell: { width, height } } } } },
  };
}

function makeDom(): RegistryDom {
  const dom: RegistryDom = {
    className: "",
    parentElement: null,
    remove: () => {
      dom.parentElement = null;
    },
  };
  return dom;
}

function makeParent(width = 1000, height = 600): MountPoint {
  const parent = {
    appendChild: (node: RegistryDom) => {
      node.parentElement = parent as never;
    },
    getBoundingClientRect: () => ({ width, height }),
  };
  return parent as unknown as MountPoint;
}

describe("TerminalRegistry — 셀 메트릭 변화 refit", () => {
  let frames: Array<() => void>;

  beforeEach(() => {
    frames = [];
    vi.stubGlobal("requestAnimationFrame", (cb: () => void) => {
      frames.push(cb);
      return frames.length;
    });
    vi.stubGlobal("cancelAnimationFrame", () => undefined);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  const flushFrames = (): void => {
    const pending = frames;
    frames = [];
    for (const frame of pending) frame();
  };

  function setup() {
    const terminal = cellTerminal(6);
    const cell = terminal._core._renderService.dimensions.css.cell;
    // FitAddon처럼 가용 폭을 지금 셀 폭으로 나눈다.
    const proposeDimensions = vi.fn(() => ({ cols: Math.floor(978 / cell.width), rows: 38 }));
    const registry = new TerminalRegistry({
      createTerminal: () => terminal,
      createFitAddon: () => ({ proposeDimensions }),
      createDom: makeDom,
    });
    const entry = registry.acquire("v1");
    const onFit = vi.fn();
    entry.setFitHandler(onFit);
    entry.mount(makeParent());
    return { terminal, cell, entry, onFit };
  }

  it("렌더러 교체로 셀이 넓어지면 다음 프레임에 다시 fit해 cols를 줄인다", () => {
    const { terminal, cell, onFit } = setup();
    expect(onFit).toHaveBeenLastCalledWith(expect.objectContaining({ cols: 163 }));
    onFit.mockClear();

    // WebGL(6px, 내림) → DOM(6.5px) 폴백.
    cell.width = 6.5;
    terminal.emitRender();
    expect(onFit).not.toHaveBeenCalled(); // 렌더 콜백 안에서는 격자를 바꾸지 않는다
    flushFrames();
    expect(onFit).toHaveBeenCalledTimes(1);
    expect(onFit).toHaveBeenLastCalledWith(expect.objectContaining({ cols: 150 }));
  });

  it("셀 크기가 그대로면 렌더마다 fit하지 않는다", () => {
    const { terminal, onFit } = setup();
    onFit.mockClear();
    terminal.emitRender();
    terminal.emitRender();
    flushFrames();
    expect(onFit).not.toHaveBeenCalled();
  });

  it("연속 렌더의 변화는 한 프레임의 fit 하나로 접는다", () => {
    const { terminal, cell, onFit } = setup();
    onFit.mockClear();
    cell.width = 6.5;
    terminal.emitRender();
    terminal.emitRender();
    terminal.emitRender();
    expect(frames).toHaveLength(1);
    flushFrames();
    expect(onFit).toHaveBeenCalledTimes(1);
    // 새 기준으로 다시 잰 뒤에는 더 부르지 않는다.
    terminal.emitRender();
    flushFrames();
    expect(onFit).toHaveBeenCalledTimes(1);
  });

  it("숨기면(unmount) 감시를 풀고, 다시 붙이면 되살린다", () => {
    const { terminal, cell, entry, onFit } = setup();
    entry.unmount();
    expect(terminal.renderListeners.size).toBe(0);
    onFit.mockClear();
    cell.width = 6.5;
    terminal.emitRender();
    flushFrames();
    expect(onFit).not.toHaveBeenCalled();

    entry.mount(makeParent());
    expect(terminal.renderListeners.size).toBe(1);
    expect(onFit).toHaveBeenLastCalledWith(expect.objectContaining({ cols: 150 }));
  });

  it("레이아웃이 없는 동안(크기 0)에는 셀 변화에도 헛된 fit을 되풀이하지 않는다", () => {
    const terminal = cellTerminal(6);
    const cell = terminal._core._renderService.dimensions.css.cell;
    const proposeDimensions = vi.fn(() => ({ cols: 80, rows: 24 }));
    const registry = new TerminalRegistry({
      createTerminal: () => terminal,
      createFitAddon: () => ({ proposeDimensions }),
      createDom: makeDom,
    });
    const entry = registry.acquire("v1");
    entry.mount(makeParent(0, 0));
    proposeDimensions.mockClear();
    cell.width = 6.5;
    terminal.emitRender();
    flushFrames();
    expect(proposeDimensions).not.toHaveBeenCalled();
  });
});
