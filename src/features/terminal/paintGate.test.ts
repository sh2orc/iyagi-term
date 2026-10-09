import { describe, expect, it, vi } from "vitest";
import { deferTerminalPaints } from "./paintGate";

describe("covered terminal painting", () => {
  it("paints all skipped rows with the latest grid after uncovering, then permits partial paints", () => {
    let covered = true;
    const paints: number[][] = [];
    class RenderService {
      _renderRows(start: number, end: number) { paints.push([start, end]); }
    }
    const service = new RenderService();
    const original = service._renderRows;
    const term = { rows: 24, refresh: vi.fn(), _core: { _renderService: service } };
    const gate = deferTerminalPaints(term, () => covered);
    service._renderRows(2, 5);
    service._renderRows(10, 15);
    expect(paints).toEqual([]);
    term.rows = 30;
    covered = false;
    service._renderRows(0, 0); // Cursor blink must also catch up skipped text.
    service._renderRows(3, 3);
    expect(paints).toEqual([[0, 29], [3, 3]]);
    gate.dispose();
    expect(service._renderRows).toBe(original);
    expect(Object.hasOwn(service, "_renderRows")).toBe(false);
    expect(term.refresh).not.toHaveBeenCalled(); // No extra paint after commit.
  });

  it("remains transparent to renderer replacement and disposal is idempotent", () => {
    let renderer = vi.fn();
    const service = { _renderRows(start: number, end: number) { renderer(start, end); } };
    const original = service._renderRows;
    const term = { rows: 24, refresh: vi.fn(), _core: { _renderService: service } };
    const gate = deferTerminalPaints(term, () => true);
    service._renderRows(0, 23);
    expect(renderer).not.toHaveBeenCalled();
    renderer = vi.fn(); // WebGL context loss installs the DOM fallback.
    gate.dispose();
    gate.dispose();
    expect(service._renderRows).toBe(original);
    expect(term.refresh.mock.calls).toEqual([[0, 23]]);
    service._renderRows(0, 23);
    expect(renderer.mock.calls).toEqual([[0, 23]]);
  });

  it("falls back to the snapshot alone if xterm's private surface is unavailable", () => {
    const term = { rows: 24, refresh: vi.fn() };
    const gate = deferTerminalPaints(term, () => true);
    expect(() => gate.dispose()).not.toThrow();
    expect(term.refresh).not.toHaveBeenCalled();
  });
});
