// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { beginZoomPreview, cancelZoomPreview, observeHistoryRedraws, zoomPreviewFor } from "./zoomPreview";

function terminal() {
  const element = document.createElement("div");
  element.innerHTML = '<div class="xterm-screen"><div>current prompt</div></div>';
  document.body.appendChild(element);
  const handlers = new Map<string, (params: number[]) => boolean>();
  const renderers = new Set<() => void>();
  const rendered = vi.fn(() => { for (const handler of renderers) handler(); });
  const renderService = { _renderRows: rendered };
  const dispose = vi.fn();
  return {
    element, cols: 80, rows: 24, options: { fontSize: 13 },
    buffer: { active: { viewportY: 100, baseY: 100 } },
    modes: { synchronizedOutputMode: false }, refresh: vi.fn(),
    onRender: (handler: () => void) => {
      renderers.add(handler);
      return { dispose: () => renderers.delete(handler) };
    },
    _core: { _renderService: renderService }, rendered,
    paint: () => renderService._renderRows(),
    parser: { registerCsiHandler: (identifier: { final: string }, handler: (params: number[]) => boolean) => {
      handlers.set(identifier.final, handler);
      return { dispose };
    } }, handlers, dispose,
  };
}

describe("zoom preview lifecycle", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal("requestAnimationFrame", (callback: () => void) => setTimeout(callback, 16));
    vi.stubGlobal("cancelAnimationFrame", (id: ReturnType<typeof setTimeout>) => clearTimeout(id));
  });
  afterEach(() => {
    document.body.replaceChildren();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("reveals a quiet shell after the grid is ready, even without resize output", () => {
    const term = terminal();
    beginZoomPreview(term);
    zoomPreviewFor(term)!.expectResize(80, 24);
    expect(term.element.querySelector(".terminal-zoom-preview")).not.toBeNull();
    vi.advanceTimersByTime(8);
    term.paint();
    expect(term.refresh).toHaveBeenCalledWith(0, 23);
    expect(term.element.querySelector(".terminal-zoom-preview")).toBeNull();
    vi.advanceTimersByTime(5000);
    expect(zoomPreviewFor(term)).toBeUndefined();
  });

  it("synchronizes the viewport before painting, without moving it again on reveal", () => {
    const calls: string[] = [];
    const term = Object.assign(terminal(), {
      scrollToLine: () => calls.push("viewport"),
      refresh: () => calls.push("paint-request"),
    });
    beginZoomPreview(term);
    zoomPreviewFor(term)!.expectResize(80, 24);
    expect(calls).toEqual(["viewport", "paint-request"]);
    term.paint();
    expect(term.element.querySelector(".terminal-zoom-preview")).toBeNull();
    expect(calls).toEqual(["viewport", "paint-request"]);
    cancelZoomPreview(term);
  });

  it("stops forcing full refreshes after reveal and expires its observer despite ordinary output", () => {
    const sync = vi.fn();
    const term = Object.assign(terminal(), { scrollToLine: sync });
    const originalRender = term._core._renderService._renderRows;
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(80, 24);
    term.paint();
    term.refresh.mockClear();
    sync.mockClear();
    for (let i = 0; i < 100; i++) {
      preview.outputPending();
      preview.outputIdle();
      term.paint();
      vi.advanceTimersByTime(40);
    }
    expect(term.refresh).not.toHaveBeenCalled();
    expect(sync).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1000);
    expect(zoomPreviewFor(term)).toBeUndefined();
    expect(term._core._renderService._renderRows).toBe(originalRender);
  });

  it("waits for the latest requested grid and all queued output", () => {
    const term = terminal();
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(70, 20);
    preview.resizeApplied(75, 22);
    vi.advanceTimersByTime(200);
    expect(zoomPreviewFor(term)).toBe(preview);
    preview.outputPending();
    preview.resizeApplied(70, 20);
    vi.advanceTimersByTime(200);
    expect(zoomPreviewFor(term)).toBe(preview);
    preview.outputIdle();
    term.paint();
    expect(term.element.querySelector(".terminal-zoom-preview")).toBeNull();
    cancelZoomPreview(term);
  });

  it("keeps a long synchronized frame covered after xterm's watchdog expires", () => {
    const term = terminal();
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(80, 24);
    preview.outputPending();
    term.handlers.get("J")!([3]);
    expect(term.handlers.get("h")!([2026])).toBe(false); // Do not consume the protocol command.
    preview.outputIdle();
    term.modes.synchronizedOutputMode = false; // xterm's watchdog; the actual frame is still open.
    vi.advanceTimersByTime(1500);
    expect(zoomPreviewFor(term)).toBe(preview);
    term.handlers.get("l")!([2026]);
    preview.outputIdle();
    term.paint();
    expect(zoomPreviewFor(term)).toBeUndefined();
  });

  it("cancels a scheduled reveal when more output arrives before paint", () => {
    const term = terminal();
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(80, 24);
    term.handlers.get("J")!([2]);
    preview.outputIdle();
    vi.advanceTimersByTime(116);
    preview.outputPending();
    term.paint();
    expect(term.rendered).not.toHaveBeenCalled();
    vi.advanceTimersByTime(100);
    expect(zoomPreviewFor(term)).toBe(preview);
    preview.outputIdle();
    vi.advanceTimersByTime(100);
    // A timer or RAF alone does not prove that the renderer has painted.
    expect(term.element.querySelector(".terminal-zoom-preview")).not.toBeNull();
    term.paint();
    expect(term.element.querySelector(".terminal-zoom-preview")).toBeNull();
    cancelZoomPreview(term);
  });

  it("resumes painting all skipped rows when user input cancels an unfinished resize", () => {
    const term = terminal();
    const originalRender = term._core._renderService._renderRows;
    beginZoomPreview(term);
    zoomPreviewFor(term)!.expectResize(90, 30);
    for (let i = 0; i < 4; i++) term.paint();
    expect(term.rendered).not.toHaveBeenCalled();
    term.element.dispatchEvent(new WheelEvent("wheel"));
    expect(term._core._renderService._renderRows).toBe(originalRender);
    expect(term.refresh).toHaveBeenCalledWith(0, 23);
    term.paint();
    expect(term.rendered).toHaveBeenCalledOnce();
    expect(zoomPreviewFor(term)).toBeUndefined();
  });

  it("reveals a completed frame on paint without waiting for continuous animation to stop", () => {
    const term = terminal();
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(80, 24);
    preview.outputPending();
    term.handlers.get("J")!([3]);
    term.handlers.get("h")!([2026]);
    term.handlers.get("l")!([2026]);
    preview.outputIdle();
    expect(zoomPreviewFor(term)).toBe(preview); // Buffer completion alone is not a paint.
    vi.advanceTimersByTime(8);
    term.paint();
    expect(zoomPreviewFor(term)).toBeUndefined();
  });

  it("does not reveal an old frame that spans the latest resize", () => {
    const term = terminal();
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(70, 20);
    preview.outputPending();
    term.handlers.get("h")!([2026]);
    preview.resizeApplied(70, 20);
    term.handlers.get("l")!([2026]);
    preview.outputIdle();
    term.paint();
    expect(zoomPreviewFor(term)).toBe(preview);
    expect(term.element.querySelector(".terminal-zoom-preview")).not.toBeNull();
    preview.outputPending();
    term.handlers.get("J")!([3]);
    term.handlers.get("h")!([2026]);
    term.handlers.get("l")!([2026]);
    preview.outputIdle();
    term.paint();
    expect(zoomPreviewFor(term)).toBeUndefined();
  });

  it("does not uncover partial output that follows a completed frame before paint", () => {
    const term = terminal();
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(80, 24);
    preview.outputPending();
    term.handlers.get("h")!([2026]);
    term.handlers.get("l")!([2026]);
    preview.outputIdle();
    preview.outputPending(); // An unsynchronized clear/rebuild starts before the frame paints.
    preview.outputIdle();
    term.paint();
    expect(zoomPreviewFor(term)).toBe(preview);
    cancelZoomPreview(term);
  });

  it("keeps covering a slow redraw while output progresses, then times out a stalled frame", () => {
    const term = terminal();
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(80, 24);
    term.handlers.get("h")!([2026]);
    for (let i = 0; i < 4; i++) {
      preview.outputPending();
      preview.outputIdle();
      vi.advanceTimersByTime(2000);
      expect(zoomPreviewFor(term)).toBe(preview);
    }
    vi.advanceTimersByTime(3000);
    expect(zoomPreviewFor(term)).toBeUndefined();
  });

  it.each(["timeout", "unmount"])("releases the overlay and subscriptions on %s", (reason) => {
    const term = terminal();
    beginZoomPreview(term);
    if (reason === "timeout") vi.advanceTimersByTime(5000);
    else cancelZoomPreview(term);
    expect(zoomPreviewFor(term)).toBeUndefined();
    expect(term.element.querySelector(".terminal-zoom-preview")).toBeNull();
    expect(term.dispose).toHaveBeenCalledTimes(3);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("keeps repeated zoom immediate but yields to typing or scrolling", () => {
    const term = terminal();
    beginZoomPreview(term);
    const first = zoomPreviewFor(term);
    term.element.dispatchEvent(new KeyboardEvent("keydown", { key: "Meta", metaKey: true }));
    expect(zoomPreviewFor(term)).toBe(first);
    term.element.dispatchEvent(new KeyboardEvent("keydown", { key: "+", metaKey: true }));
    beginZoomPreview(term);
    expect(zoomPreviewFor(term)).toBe(first);
    term.element.dispatchEvent(new KeyboardEvent("keydown", { key: "Unidentified", code: "NumpadAdd", metaKey: true }));
    expect(zoomPreviewFor(term)).toBe(first);
    term.element.dispatchEvent(new KeyboardEvent("keydown", { key: "a" }));
    expect(zoomPreviewFor(term)).toBeUndefined();
    beginZoomPreview(term);
    term.element.dispatchEvent(new WheelEvent("wheel"));
    expect(zoomPreviewFor(term)).toBeUndefined();
  });

  it("releases the preview if its renderer disappears before the final viewport sync", () => {
    const term = Object.assign(terminal(), {
      scrollToLine: () => { throw new Error("renderer unavailable"); },
    });
    beginZoomPreview(term);
    expect(() => cancelZoomPreview(term)).not.toThrow();
    expect(term.element.querySelector(".terminal-zoom-preview")).toBeNull();
    expect(zoomPreviewFor(term)).toBeUndefined();
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each(["frame", "screen-clear", "quiet"])("covers a delayed history rebuild after presenting an earlier %s", (kind) => {
    const term = terminal();
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(80, 24);
    if (kind !== "quiet") {
      preview.outputPending();
      if (kind === "screen-clear") term.handlers.get("J")!([2]);
      term.handlers.get("h")!([2026]);
      term.handlers.get("l")!([2026]);
      preview.outputIdle();
      term.paint();
    } else {
      term.paint();
    }
    expect(term.element.querySelector(".terminal-zoom-preview")).toBeNull();
    expect(zoomPreviewFor(term)).toBe(preview);
    term.options.fontSize = 12;
    term.element.querySelector(".xterm-screen")!.textContent = "newly painted prompt";
    vi.advanceTimersByTime(250);
    preview.outputPending();
    expect(term.handlers.get("J")!([2])).toBe(false);
    const overlay = term.element.querySelector(".terminal-zoom-preview");
    expect(overlay?.textContent).toBe("newly painted prompt");
    term.handlers.get("J")!([3]);
    term.handlers.get("h")!([2026]);
    preview.outputIdle();
    vi.advanceTimersByTime(1500);
    expect(overlay?.isConnected).toBe(true);
    preview.outputPending();
    term.handlers.get("l")!([2026]);
    preview.outputIdle();
    term.paint();
    expect(overlay?.isConnected).toBe(false);
    expect(zoomPreviewFor(term)).toBeUndefined();
  });

  it("captures the current screen when zoom resumes after an earlier frame was presented", () => {
    const term = terminal();
    beginZoomPreview(term);
    zoomPreviewFor(term)!.expectResize(80, 24);
    term.paint();
    term.options.fontSize = 12;
    term.element.querySelector(".xterm-screen")!.textContent = "updated prompt";
    beginZoomPreview(term);
    const screen = term.element.querySelector<HTMLElement>(".terminal-zoom-preview .xterm-screen");
    expect(screen?.textContent).toBe("updated prompt");
    expect(screen?.style.transform).toBe("none");
    cancelZoomPreview(term);
  });

  it("still allows zoom when the screen disappears before a watched preview is recaptured", () => {
    const term = terminal();
    beginZoomPreview(term);
    zoomPreviewFor(term)!.expectResize(80, 24);
    vi.advanceTimersByTime(132);
    term.element.replaceChildren();
    expect(() => beginZoomPreview(term)).not.toThrow();
    expect(zoomPreviewFor(term)).toBeUndefined();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not delay a shell's zoom after clearing its history", () => {
    const term = terminal();
    const stopObserving = observeHistoryRedraws(term);
    term.handlers.get("J")!([3]);
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(90, 30);
    preview.resizeApplied(90, 30);
    term.paint();
    expect(term.element.querySelector(".terminal-zoom-preview")).toBeNull();
    cancelZoomPreview(term);
    stopObserving();
  });

  it("keeps a known rebuilding CLI's original layout through its preliminary frames", () => {
    const term = terminal();
    const stopObserving = observeHistoryRedraws(term);
    term.handlers.get("h")!([2026]);
    term.handlers.get("J")!([3]); // Initial CLI render, before any zoom.
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(90, 30);
    preview.resizeApplied(90, 30);
    for (let i = 0; i < 3; i++) {
      preview.outputPending();
      term.handlers.get("h")!([2026]);
      term.handlers.get("l")!([2026]);
      preview.outputIdle();
      term.paint();
      vi.advanceTimersByTime(300); // Total exceeds the old fixed 500ms deadline.
    }
    const screen = term.element.querySelector<HTMLElement>(".terminal-zoom-preview .xterm-screen");
    expect(screen?.textContent).toBe("current prompt");
    expect(screen?.style.transform).toBe("none");
    expect(screen?.style.top).toBe("0px");
    preview.outputPending();
    term.handlers.get("J")!([3]);
    term.handlers.get("h")!([2026]);
    preview.outputIdle();
    vi.advanceTimersByTime(1000); // The start deadline must not uncover an active rebuild.
    expect(screen?.isConnected).toBe(true);
    preview.outputPending();
    term.handlers.get("l")!([2026]);
    preview.outputIdle();
    term.paint();
    expect(zoomPreviewFor(term)).toBeUndefined();
    stopObserving();
  });

  it("waits across a separate history clear and synchronized redraw", () => {
    const term = terminal();
    const stopObserving = observeHistoryRedraws(term);
    term.handlers.get("h")!([2026]);
    term.handlers.get("J")!([3]);
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(90, 30);
    preview.resizeApplied(90, 30);
    term.paint();
    expect(term.element.querySelector(".terminal-zoom-preview")).not.toBeNull();
    preview.outputPending();
    term.handlers.get("J")!([2]);
    term.handlers.get("J")!([3]);
    preview.outputIdle();
    vi.advanceTimersByTime(200);
    term.paint();
    expect(term.element.querySelector(".terminal-zoom-preview")).not.toBeNull();
    preview.outputPending();
    term.handlers.get("h")!([2026]);
    preview.outputIdle();
    vi.advanceTimersByTime(1500);
    expect(term.element.querySelector(".terminal-zoom-preview")).not.toBeNull();
    preview.outputPending();
    term.handlers.get("l")!([2026]);
    preview.outputIdle();
    term.paint();
    expect(term.element.querySelector(".terminal-zoom-preview")).toBeNull();
    stopObserving();
  });

  it("bounds the wait when a CLI no longer rebuilds history", () => {
    const term = terminal();
    const stopObserving = observeHistoryRedraws(term);
    term.handlers.get("J")!([3]);
    term.handlers.get("h")!([2026]);
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(90, 30);
    preview.resizeApplied(90, 30);
    vi.advanceTimersByTime(499);
    term.paint();
    expect(term.element.querySelector(".terminal-zoom-preview")).not.toBeNull();
    vi.advanceTimersByTime(1);
    term.paint();
    expect(term.element.querySelector(".terminal-zoom-preview")).toBeNull();
    cancelZoomPreview(term);
    stopObserving();
  });
});

describe("zoom preview reveal group (window resize across panes)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal("requestAnimationFrame", (callback: () => void) => setTimeout(callback, 16));
    vi.stubGlobal("cancelAnimationFrame", (id: ReturnType<typeof setTimeout>) => clearTimeout(id));
  });
  afterEach(() => {
    document.body.replaceChildren();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  const covered = (term: ReturnType<typeof terminal>) =>
    term.element.querySelector(".terminal-zoom-preview") !== null;

  it("uncovers panes resized together in the same pass, not one by one", () => {
    const fast = terminal();
    const slow = terminal();
    beginZoomPreview(fast);
    beginZoomPreview(slow);
    zoomPreviewFor(fast)!.expectResize(80, 24);
    zoomPreviewFor(slow)!.expectResize(100, 30); // grid not applied yet
    fast.paint();
    expect(covered(fast)).toBe(true); // ready, but waits for its sibling
    zoomPreviewFor(slow)!.resizeApplied(100, 30);
    expect(covered(fast)).toBe(true);
    slow.paint();
    expect(covered(fast)).toBe(false);
    expect(covered(slow)).toBe(false);
    cancelZoomPreview(fast);
    cancelZoomPreview(slow);
  });

  it("does not let a slow sibling hold a ready pane past the cap", () => {
    const fast = terminal();
    const slow = terminal();
    beginZoomPreview(fast);
    beginZoomPreview(slow);
    zoomPreviewFor(fast)!.expectResize(80, 24);
    zoomPreviewFor(slow)!.expectResize(100, 30);
    fast.paint();
    vi.advanceTimersByTime(119); // GROUP_REVEAL_CAP_MS(120) 직전
    expect(covered(fast)).toBe(true);
    vi.advanceTimersByTime(1);
    expect(covered(fast)).toBe(false);
    expect(covered(slow)).toBe(true); // still waiting for its own grid
    // Released from the group: its own paint now reveals it immediately.
    zoomPreviewFor(slow)!.resizeApplied(100, 30);
    slow.paint();
    expect(covered(slow)).toBe(false);
    cancelZoomPreview(fast);
    cancelZoomPreview(slow);
  });

  it("a sibling dismissed by input releases the rest at once", () => {
    const fast = terminal();
    const slow = terminal();
    beginZoomPreview(fast);
    beginZoomPreview(slow);
    zoomPreviewFor(fast)!.expectResize(80, 24);
    zoomPreviewFor(slow)!.expectResize(100, 30);
    fast.paint();
    expect(covered(fast)).toBe(true);
    slow.element.dispatchEvent(new Event("pointerdown"));
    expect(covered(fast)).toBe(false);
    cancelZoomPreview(fast);
  });

  it("previews begun far apart are separate groups", () => {
    const first = terminal();
    beginZoomPreview(first);
    vi.advanceTimersByTime(200);
    const later = terminal();
    beginZoomPreview(later);
    zoomPreviewFor(first)!.expectResize(80, 24);
    first.paint();
    expect(covered(first)).toBe(false);
    cancelZoomPreview(later);
  });

  it("records the stage timings of each resize on window.__resizeTrace", () => {
    const view = window as unknown as { __resizeTrace?: Array<Record<string, unknown>> };
    view.__resizeTrace = [];
    const term = terminal();
    beginZoomPreview(term);
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(100, 30);
    vi.advanceTimersByTime(30);
    preview.resizeAcknowledged();
    vi.advanceTimersByTime(10);
    preview.resizeApplied(100, 30);
    term.paint();
    expect(view.__resizeTrace).toHaveLength(1);
    expect(view.__resizeTrace![0]).toMatchObject({ cols: 100, rows: 30, outcome: "painted", group: 1 });
    for (const stage of ["rpcAck", "applied", "ready", "revealed"]) {
      expect(typeof view.__resizeTrace![0][stage]).toBe("number");
    }
    expect(typeof view.__resizeTrace![0].captureMs).toBe("number");
    cancelZoomPreview(term);
  });
});

describe("zoom preview waits for the program's SIGWINCH redraw", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal("requestAnimationFrame", (callback: () => void) => setTimeout(callback, 16));
    vi.stubGlobal("cancelAnimationFrame", (id: ReturnType<typeof setTimeout>) => clearTimeout(id));
  });
  afterEach(() => {
    document.body.replaceChildren();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  const covered = (term: ReturnType<typeof terminal>) =>
    term.element.querySelector(".terminal-zoom-preview") !== null;

  /** Journal resize to 100x30: xterm's grid follows, then the PTY signals. */
  function applyLiveResize(term: ReturnType<typeof terminal>) {
    const preview = zoomPreviewFor(term)!;
    preview.expectResize(100, 30);
    Object.assign(term, { cols: 100, rows: 30 });
    preview.resizeApplied(100, 30);
    preview.resizeSignaled();
    return preview;
  }

  it("does not reveal xterm's interim reflow before the program answers", () => {
    const term = terminal();
    beginZoomPreview(term);
    const preview = applyLiveResize(term);
    term.paint();
    expect(covered(term)).toBe(true);
    // The redraw arrives and settles: reveal after the quiet period's paint.
    preview.outputPending();
    preview.outputIdle();
    vi.advanceTimersByTime(39); // QUIET_MS(40) 직전 — 아직 덮어 둔다
    term.paint();
    expect(covered(term)).toBe(true);
    vi.advanceTimersByTime(1);
    term.paint();
    expect(covered(term)).toBe(false);
    const trace = (window as unknown as { __resizeTrace: Array<Record<string, number>> }).__resizeTrace.at(-1)!;
    expect(typeof trace.winchResponse).toBe("number");
    cancelZoomPreview(term);
  });

  it("learns a silent program and shortens the next wait", () => {
    const term = terminal();
    beginZoomPreview(term);
    const preview = applyLiveResize(term);
    // 첫 대기는 전체 예산(90ms)만큼 기다렸다가 공개한다.
    vi.advanceTimersByTime(89);
    term.paint();
    expect(covered(term)).toBe(true);
    vi.advanceTimersByTime(1);
    term.paint();
    expect(covered(term)).toBe(false);

    // 같은 터미널의 다음 확대: 조용한 프로그램은 30ms만 기다린다.
    preview.zoom();
    preview.expectResize(120, 40);
    Object.assign(term, { cols: 120, rows: 40 });
    preview.resizeApplied(120, 40);
    preview.resizeSignaled();
    vi.advanceTimersByTime(29);
    term.paint();
    expect(covered(term)).toBe(true);
    vi.advanceTimersByTime(1);
    term.paint();
    expect(covered(term)).toBe(false);
    cancelZoomPreview(term);
  });

  it("learns an answering program and bounds the next wait by its latency", () => {
    const term = terminal();
    beginZoomPreview(term);
    const preview = applyLiveResize(term);
    // 답이 (가짜 시계 밖 실제 시계로) 즉시 왔다 — 학습값은 하한(30ms)에 붙는다.
    preview.outputPending();
    preview.outputIdle();
    vi.advanceTimersByTime(40);
    term.paint();
    expect(covered(term)).toBe(false);

    preview.zoom();
    preview.expectResize(120, 40);
    Object.assign(term, { cols: 120, rows: 40 });
    preview.resizeApplied(120, 40);
    preview.resizeSignaled();
    vi.advanceTimersByTime(29);
    term.paint();
    expect(covered(term)).toBe(true);
    vi.advanceTimersByTime(1);
    term.paint();
    expect(covered(term)).toBe(false);
    cancelZoomPreview(term);
  });

  it("reveals a silent program after the bounded wait", () => {
    const term = terminal();
    beginZoomPreview(term);
    applyLiveResize(term);
    vi.advanceTimersByTime(89);
    term.paint();
    expect(covered(term)).toBe(true);
    vi.advanceTimersByTime(1);
    term.paint();
    expect(covered(term)).toBe(false);
    cancelZoomPreview(term);
  });

  it("records output that lands after the reveal as a late redraw", () => {
    const term = terminal();
    beginZoomPreview(term);
    const preview = applyLiveResize(term);
    vi.advanceTimersByTime(90);
    term.paint();
    expect(covered(term)).toBe(false);
    vi.advanceTimersByTime(25);
    preview.outputPending();
    const trace = (window as unknown as { __resizeTrace: Array<Record<string, number>> }).__resizeTrace.at(-1)!;
    expect(typeof trace.lateOutput).toBe("number"); // performance.now is not faked
    cancelZoomPreview(term);
  });
});

describe("SIGWINCH 무응답 학습 램프와 TUI 리셋", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal("requestAnimationFrame", (callback: () => void) => setTimeout(callback, 16));
    vi.stubGlobal("cancelAnimationFrame", (id: ReturnType<typeof setTimeout>) => clearTimeout(id));
  });
  afterEach(() => {
    document.body.replaceChildren();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  const covered = (term: ReturnType<typeof terminal>) =>
    term.element.querySelector(".terminal-zoom-preview") !== null;

  /** 조용한 프로그램의 resize 한 트랜잭션: waitMs 뒤에 공개된다. */
  function silentResize(term: ReturnType<typeof terminal>, cols: number, rows: number) {
    const preview = zoomPreviewFor(term)!;
    preview.zoom();
    preview.expectResize(cols, rows);
    Object.assign(term, { cols, rows });
    preview.resizeApplied(cols, rows);
    preview.resizeSignaled();
    return preview;
  }

  it("무응답이 쌓일수록 대기가 내려간다(90 → 30 → 12ms)", () => {
    const term = terminal();
    beginZoomPreview(term);
    // 1번째: 학습 없음 — 전체 예산(90ms).
    silentResize(term, 100, 30);
    vi.advanceTimersByTime(89);
    term.paint();
    expect(covered(term)).toBe(true);
    vi.advanceTimersByTime(1);
    term.paint();
    expect(covered(term)).toBe(false);
    // 2번째: 첫 무응답 — 신중한 30ms.
    silentResize(term, 104, 31);
    vi.advanceTimersByTime(29);
    term.paint();
    expect(covered(term)).toBe(true);
    vi.advanceTimersByTime(1);
    term.paint();
    expect(covered(term)).toBe(false);
    // 3번째: 연속 무응답 — 바닥 12ms.
    silentResize(term, 108, 32);
    vi.advanceTimersByTime(11);
    term.paint();
    expect(covered(term)).toBe(true);
    vi.advanceTimersByTime(1);
    term.paint();
    expect(covered(term)).toBe(false);
    cancelZoomPreview(term);
  });

  it("동기화 출력 프레임(2026)을 보면 무응답 학습을 버리고 예산을 되돌린다", () => {
    const term = terminal();
    beginZoomPreview(term);
    // 무응답을 두 번 쌓아 바닥(12ms)까지 내려간 상태를 만든다.
    silentResize(term, 100, 30);
    vi.advanceTimersByTime(90);
    term.paint();
    silentResize(term, 104, 31);
    vi.advanceTimersByTime(90);
    term.paint();
    expect(covered(term)).toBe(false);

    // 이 pane에서 TUI가 돌기 시작해 동기화 프레임을 썼다 (h … l).
    term.handlers.get("h")!([2026]);
    term.handlers.get("l")!([2026]);

    // 다음 resize는 학습 없음과 같은 전체 예산(90ms)을 다시 쓴다.
    silentResize(term, 108, 32);
    vi.advanceTimersByTime(29);
    term.paint();
    expect(covered(term)).toBe(true);
    vi.advanceTimersByTime(60);
    term.paint();
    expect(covered(term)).toBe(true); // 아직 90ms 예산 안이다
    vi.advanceTimersByTime(1);
    term.paint();
    expect(covered(term)).toBe(false);
    cancelZoomPreview(term);
  });
});

