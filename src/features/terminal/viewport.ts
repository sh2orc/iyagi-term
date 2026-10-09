/** xterm buffer APIs needed to keep the same text in view during zoom/reflow. */
export interface ViewportTerminal {
  cols?: number;
  buffer?: {
    active: {
      type: "normal" | "alternate";
      baseY: number;
      viewportY: number;
      cursorY: number;
      getLine(index: number): { isWrapped: boolean } | undefined;
    };
  };
  registerMarker?(cursorYOffset: number): { line: number; isDisposed: boolean; dispose(): void } | undefined;
  scrollToLine?(line: number): void;
}

function scrollToAnchor(terminal: ViewportTerminal, line: number): void {
  // xterm 6's public scrollToLine adds a row delta to the OLD pixel offset.
  // Immediately after font/reflow changes those units no longer agree, and
  // queueSync can also retain _latestYDisp from before the resize. Synchronize
  // dimensions and set an absolute, non-animated position in the viewport.
  // Keep this version-specific adapter isolated, with a public-API fallback.
  const viewport = (terminal as { _core?: { _viewport?: {
    _sync?(): void;
    scrollToLine?(line: number, disableSmoothScroll: boolean): void;
  } } })._core?._viewport;
  if (viewport?._sync && viewport.scrollToLine) {
    viewport._sync();
    viewport.scrollToLine(line, true);
  } else {
    terminal.scrollToLine?.(line);
  }
}

/** Commit the current buffer position before revealing a completed zoom. */
export function syncViewport(terminal: ViewportTerminal): void {
  const buffer = terminal.buffer?.active;
  if (buffer) scrollToAnchor(terminal, buffer.viewportY);
}

function lineIdentity(line: { isWrapped: boolean } | undefined): object | undefined {
  // xterm 6's API creates a new wrapper on every getLine call, but its backing
  // BufferLine survives reflow. Markers can miscount inserted rows when a
  // narrower grid overflows the scrollback limit; the retained line is exact.
  return (line as { _line?: object } | undefined)?._line;
}

/**
 * xterm adjusts viewportY with the screen's row count during resize, even when
 * the reader is in scrollback. Anchor the logical line instead: markers follow
 * reflow and scrollback trimming, whereas a saved numeric viewportY does not.
 * The live bottom and alternate screen retain xterm's cursor-based behavior.
 */
export function preserveViewport(terminal: ViewportTerminal, change: () => void): void {
  const buffer = terminal.buffer?.active;
  if (!buffer || buffer.type !== "normal" || buffer.viewportY >= buffer.baseY ||
    !terminal.registerMarker || !terminal.scrollToLine) {
    change();
    return;
  }
  let line = buffer.viewportY;
  // A continuation row can disappear when columns grow. Mark the start of
  // the wrapped line and retain the reader's approximate character offset.
  while (line > 0 && buffer.getLine(line)?.isWrapped) line--;
  const anchor = lineIdentity(buffer.getLine(line));
  const offset = (buffer.viewportY - line) * (terminal.cols ?? 1);
  const marker = terminal.registerMarker(line - buffer.baseY - buffer.cursorY);
  try {
    change();
    if (terminal.buffer?.active === buffer) {
      let target = marker && !marker.isDisposed ? marker.line : undefined;
      if (anchor && (target === undefined || lineIdentity(buffer.getLine(target)) !== anchor)) {
        target = undefined;
        for (let row = 0; ; row++) {
          const candidate = buffer.getLine(row);
          if (!candidate) break;
          if (lineIdentity(candidate) === anchor) { target = row; break; }
        }
      }
      if (target !== undefined) scrollToAnchor(terminal, target + Math.floor(offset / (terminal.cols ?? 1)));
    }
  } finally {
    marker?.dispose();
  }
}
