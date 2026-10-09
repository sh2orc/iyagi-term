import { beforeEach, describe, expect, it, vi } from "vitest";
import { TerminalRegistry, type RegistryDom, type TerminalLike } from "./registry";
import { beginZoomPreview } from "./zoomPreview";

// node 가짜 터미널(element 없음)에서는 beginZoomPreview가 본체에서 조용히
// no-op라 호출 여부만 관찰할 수 있다 — 실제 동작은 그대로 흘러가게 감싼다.
vi.mock("./zoomPreview", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./zoomPreview")>();
  return { ...actual, beginZoomPreview: vi.fn(actual.beginZoomPreview) };
});

function fakeTerminal(initialFontSize = 13): TerminalLike {
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

function fakeDom(): RegistryDom {
  return {
    className: "",
    parentElement: null,
    remove: () => undefined,
  };
}

describe("TerminalRegistry.setFontSize (pane zoom)", () => {
  it("applies a new size and returns true", () => {
    const registry = new TerminalRegistry({ createTerminal: () => fakeTerminal(), createDom: fakeDom });
    const entry = registry.acquire("v1");
    expect(entry.setFontSize(18)).toBe(true);
    expect(entry.terminal.options?.fontSize).toBe(18);
  });

  it("is a no-op for the same size (returns false)", () => {
    const registry = new TerminalRegistry({
      createTerminal: () => fakeTerminal(13),
      createDom: fakeDom,
    });
    const entry = registry.acquire("v1");
    expect(entry.setFontSize(13)).toBe(false);
  });

  it("no-ops when the terminal has no live options surface (node fakes)", () => {
    const bare = { ...fakeTerminal(), options: undefined };
    const registry = new TerminalRegistry({ createTerminal: () => bare, createDom: fakeDom });
    const entry = registry.acquire("v1");
    expect(entry.setFontSize(20)).toBe(false);
  });

  it("refits after a size change so cols/rows reflow to the fit handler", () => {
    const onFit = vi.fn();
    const registry = new TerminalRegistry({
      createTerminal: () => fakeTerminal(),
      createFitAddon: () => ({ proposeDimensions: () => ({ cols: 80, rows: 24 }) }),
      createDom: fakeDom,
    });
    const entry = registry.acquire("v1");
    entry.setFitHandler(onFit);
    entry.mount({ appendChild: () => undefined }); // mount 시 refit 1회
    onFit.mockClear();

    expect(entry.setFontSize(20)).toBe(true);
    expect(onFit).toHaveBeenCalledTimes(1);
    expect(onFit).toHaveBeenCalledWith(expect.objectContaining({ cols: 80, rows: 24 }));
  });

  it("does not refit when the size did not change", () => {
    const onFit = vi.fn();
    const registry = new TerminalRegistry({
      createTerminal: () => fakeTerminal(13),
      createFitAddon: () => ({ proposeDimensions: () => ({ cols: 80, rows: 24 }) }),
      createDom: fakeDom,
    });
    const entry = registry.acquire("v1");
    entry.setFitHandler(onFit);
    entry.mount({ appendChild: () => undefined });
    onFit.mockClear();

    expect(entry.setFontSize(13)).toBe(false);
    expect(onFit).not.toHaveBeenCalled();
  });
});

describe("TerminalRegistry.refit guard", () => {
  it("never fits a terminal that is not mounted — a hidden tab's detached host would yield NaN sizes", () => {
    const onFit = vi.fn();
    const registry = new TerminalRegistry({
      createTerminal: () => fakeTerminal(),
      createFitAddon: () => ({ proposeDimensions: () => ({ cols: 80, rows: 24 }) }),
      createDom: fakeDom,
    });
    const entry = registry.acquire("v1");
    entry.setFitHandler(onFit);
    entry.refit();
    expect(entry.setFontSize(20)).toBe(true); // 전역 글꼴 변경은 숨은 pane에도 닿는다
    expect(onFit).not.toHaveBeenCalled();

    entry.mount({ appendChild: () => undefined });
    expect(onFit).toHaveBeenCalledTimes(1);
    entry.unmount();
    entry.refit();
    expect(onFit).toHaveBeenCalledTimes(1);
  });
});

describe("TerminalRegistry 첫 fit 덮음(숨은 동안 크기가 바뀐 탭 복귀)", () => {
  beforeEach(() => {
    vi.mocked(beginZoomPreview).mockClear();
  });

  /**
   * 줄 내용을 주면 buffer를 가진 가짜(공개 API getLine/translateToString 모양)를
   * 만든다. 생략하면 buffer 없는 가짜 - 판정을 못 하는 경로(안전하게 덮는 쪽).
   */
  function fakeBuffer(lines: string[]) {
    return {
      active: {
        type: "normal" as const,
        baseY: 0,
        viewportY: 0,
        cursorY: 0,
        getLine: (index: number) =>
          lines[index] === undefined
            ? undefined
            : { isWrapped: false, translateToString: () => lines[index] },
      },
    };
  }

  /** mount는 lastFit을 비우므로, 처음 fit의 기준은 터미널의 현재 격자다. */
  function gridHarness(terminalCols: number, terminalRows: number, lines?: string[]) {
    let dims = { cols: terminalCols, rows: terminalRows };
    const terminal: TerminalLike = {
      ...fakeTerminal(),
      cols: terminalCols,
      rows: terminalRows,
      ...(lines ? { buffer: fakeBuffer(lines) } : {}),
    };
    const registry = new TerminalRegistry({
      createTerminal: () => terminal,
      createFitAddon: () => ({ proposeDimensions: () => dims }),
      createDom: fakeDom,
    });
    const entry = registry.acquire("v1");
    entry.setFitHandler(() => undefined);
    return {
      entry,
      terminal,
      /** host(FitAddon)가 제안하는 치수를 바꾼다 — 창 크기 변화를 모방. */
      resizeHost: (cols: number, rows: number): void => {
        dims = { cols, rows };
      },
    };
  }

  it("lastFit이 없어도 터미널 격자와 다른 치수면 미리보기로 덮는다", () => {
    const h = gridHarness(120, 30);
    h.resizeHost(150, 35); // 숨은 동안 창이 커졌다가 돌아오는 형태
    h.entry.mount({ appendChild: () => undefined });
    expect(beginZoomPreview).toHaveBeenCalledTimes(1);
    expect(beginZoomPreview).toHaveBeenCalledWith(h.terminal);
  });

  it("현재 격자와 같은 치수로 돌아오는 탭은 미리보기를 시작하지 않는다", () => {
    const h = gridHarness(80, 24);
    h.entry.mount({ appendChild: () => undefined }); // dims = 현재 격자
    expect(beginZoomPreview).not.toHaveBeenCalled();
  });

  it("빈 화면(출력이 없는 새 pane)의 첫 fit은 덧침하지 않는다 — 첫 페인트가 늦지 않는다", () => {
    const h = gridHarness(80, 24, []); // buffer는 있지만 뷰포트가 전부 빈 줄
    h.resizeHost(120, 36);
    h.entry.mount({ appendChild: () => undefined });
    expect(beginZoomPreview).not.toHaveBeenCalled();
  });

  it("프롬프트 한 줄이라도 찍혀 있으면 빈 화면이 아니므로 덮는다", () => {
    const h = gridHarness(80, 24, ["", "$ "]);
    h.resizeHost(120, 36);
    h.entry.mount({ appendChild: () => undefined });
    expect(beginZoomPreview).toHaveBeenCalledTimes(1);
    expect(beginZoomPreview).toHaveBeenCalledWith(h.terminal);
  });

  it("mount가 lastFit을 리셋해도 격자가 바뀌었으면 재부착 fit을 덮는다", () => {
    const h = gridHarness(100, 30);
    h.entry.mount({ appendChild: () => undefined }); // 같은 격자 — 덮지 않는다
    expect(beginZoomPreview).not.toHaveBeenCalled();

    h.entry.unmount(); // lastFit = null (탭 숨김)
    h.resizeHost(120, 40); // 숨은 동안 host 크기 변화
    h.entry.mount({ appendChild: () => undefined }); // 재부착 — 아직 100x30
    expect(beginZoomPreview).toHaveBeenCalledTimes(1);
  });
});

describe("TerminalRegistry 붙기 전 fit(핸들러 없는 첫 mount)", () => {
  beforeEach(() => {
    vi.mocked(beginZoomPreview).mockClear();
  });

  it("핸들러가 없으면 xterm 격자를 host에 곧바로 맞추고 미리보기는 시작하지 않는다", () => {
    const resize = vi.fn();
    const terminal: TerminalLike = { ...fakeTerminal(), cols: 80, rows: 24, resize };
    const registry = new TerminalRegistry({
      createTerminal: () => terminal,
      createFitAddon: () => ({ proposeDimensions: () => ({ cols: 132, rows: 40 }) }),
      createDom: fakeDom,
    });
    const entry = registry.acquire("v1");
    expect(entry.measure()).toBeNull(); // mount 전에는 잴 수 없다
    entry.mount({ appendChild: () => undefined });
    expect(resize).toHaveBeenCalledWith(132, 40);
    expect(beginZoomPreview).not.toHaveBeenCalled();
    expect(entry.measure()).toEqual({ cols: 132, rows: 40 });
  });

  it("host와 같은 격자면 다시 맞추지 않는다", () => {
    const resize = vi.fn();
    const terminal: TerminalLike = { ...fakeTerminal(), cols: 100, rows: 30, resize };
    const registry = new TerminalRegistry({
      createTerminal: () => terminal,
      createFitAddon: () => ({ proposeDimensions: () => ({ cols: 100, rows: 30 }) }),
      createDom: fakeDom,
    });
    registry.acquire("v1").mount({ appendChild: () => undefined });
    expect(resize).not.toHaveBeenCalled();
  });
});
