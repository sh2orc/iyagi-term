import { describe, expect, it, vi } from "vitest";
import { preserveViewport, syncViewport, type ViewportTerminal } from "./viewport";

function fixture(viewportY = 40) {
  const buffer = { type: "normal" as "normal" | "alternate", baseY: 100, viewportY, cursorY: 20,
    getLine: (_line: number) => ({ isWrapped: false }) };
  const marker = { line: viewportY, isDisposed: false, dispose: vi.fn() };
  const terminal = { cols: 80, buffer: { active: buffer },
    registerMarker: vi.fn(() => marker), scrollToLine: vi.fn() } satisfies ViewportTerminal;
  return { buffer, marker, terminal };
}

describe("preserveViewport", () => {
  it("synchronizes the scrollbar and cancels a stale scroll target before zoom is revealed", () => {
    const { buffer, terminal } = fixture(100);
    const viewport = { _sync: vi.fn(), scrollToLine: vi.fn() };
    const term = Object.assign(terminal, { _core: { _viewport: viewport } });
    buffer.viewportY = 140;
    syncViewport(term);
    expect(viewport._sync).toHaveBeenCalledOnce();
    expect(viewport.scrollToLine).toHaveBeenCalledWith(140, true);
    expect(terminal.scrollToLine).not.toHaveBeenCalled();
  });

  it("keeps the marked text in view when row changes move the viewport", () => {
    const { buffer, marker, terminal } = fixture();
    preserveViewport(terminal, () => { buffer.viewportY += 3; });
    expect(terminal.registerMarker).toHaveBeenCalledWith(-80);
    expect(terminal.scrollToLine).toHaveBeenCalledWith(40);
    expect(marker.dispose).toHaveBeenCalledOnce();
  });

  it("follows reflow and trimming instead of restoring the old row number", () => {
    const { marker, terminal } = fixture();
    preserveViewport(terminal, () => { marker.line = 65; });
    expect(terminal.scrollToLine).toHaveBeenCalledWith(65);
  });

  it("anchors wrapped text at its first row so merging does not delete the marker", () => {
    const { buffer, marker, terminal } = fixture(42);
    buffer.getLine = (line) => ({ isWrapped: line === 41 || line === 42 });
    preserveViewport(terminal, () => { terminal.cols = 160; marker.line = 25; });
    expect(terminal.registerMarker).toHaveBeenCalledWith(-80);
    expect(terminal.scrollToLine).toHaveBeenCalledWith(26);
  });

  it.each([false, true])("follows the retained line when reflow invalidates its marker (disposed: %s)", (disposed) => {
    const { buffer, marker, terminal } = fixture();
    const identity = {};
    let anchorRow = 40;
    buffer.getLine = ((row: number) => row < 100 ? { isWrapped: false, _line: row === anchorRow ? identity : {} } : undefined) as typeof buffer.getLine;
    preserveViewport(terminal, () => {
      anchorRow = 25;
      marker.line = 10;
      marker.isDisposed = disposed;
    });
    expect(terminal.scrollToLine).toHaveBeenCalledWith(25);
    expect(marker.dispose).toHaveBeenCalledOnce();
  });

  it.each(["bottom", "alternate"])("retains xterm behavior for %s", (mode) => {
    const { buffer, terminal } = fixture(mode === "bottom" ? 100 : 40);
    if (mode === "alternate") buffer.type = "alternate";
    const change = vi.fn();
    preserveViewport(terminal, change);
    expect(change).toHaveBeenCalledOnce();
    expect(terminal.registerMarker).not.toHaveBeenCalled();
    expect(terminal.scrollToLine).not.toHaveBeenCalled();
  });

  it("does not jump when the anchor was removed or the active buffer changed", () => {
    const { marker, terminal } = fixture();
    preserveViewport(terminal, () => { marker.isDisposed = true; });
    expect(terminal.scrollToLine).not.toHaveBeenCalled();
    marker.isDisposed = false;
    preserveViewport(terminal, () => { terminal.buffer.active = { ...terminal.buffer.active }; });
    expect(terminal.scrollToLine).not.toHaveBeenCalled();
    expect(marker.dispose).toHaveBeenCalledTimes(2);
  });
});
