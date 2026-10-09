interface PaintTerminal {
  rows: number;
  refresh(start: number, end: number): void;
}

interface RenderService {
  _renderRows(start: number, end: number): void;
}

/**
 * Skip covered frames without pausing parsing, write callbacks or resize metrics.
 * xterm 6 has no public paint-only suspension API. Intercept the render-service
 * entry point, including already queued RAFs, rather than the renderer itself:
 * a skipped frame must not emit onRender and masquerade as a completed paint.
 * Keep this version-specific adapter optional and independent of DOM/WebGL.
 */
export function deferTerminalPaints(terminal: PaintTerminal, covered: () => boolean): { dispose(): void } {
  const service = (terminal as PaintTerminal & {
    _core?: { _renderService?: RenderService };
  })._core?._renderService;
  if (typeof service?._renderRows !== "function") return { dispose() {} };

  const original = service._renderRows;
  const descriptor = Object.getOwnPropertyDescriptor(service, "_renderRows");
  let dirty = false;
  let disposed = false;
  function renderRows(this: RenderService, start: number, end: number): void {
    if (!disposed && covered()) {
      dirty = true;
      return;
    }
    // Skipped row ranges are no longer in xterm's debouncer. The first real
    // paint must include them all, even if triggered by only a cursor blink.
    if (dirty) {
      start = 0;
      end = terminal.rows - 1;
      dirty = false;
    }
    original.call(this, start, end);
  }
  service._renderRows = renderRows;

  return { dispose() {
    if (disposed) return;
    disposed = true;
    if (service._renderRows === renderRows) {
      if (descriptor) Object.defineProperty(service, "_renderRows", descriptor);
      else delete (service as Partial<RenderService>)._renderRows;
    }
    if (dirty) {
      dirty = false;
      // Input, unmount or a stalled redraw can cancel the preview before its
      // final paint. Never leave the live renderer displaying a skipped frame.
      terminal.refresh(0, terminal.rows - 1);
    }
  } };
}
