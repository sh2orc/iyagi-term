import type { Terminal } from "@xterm/xterm";
import { deferTerminalPaints } from "./paintGate";
import { syncViewport } from "./viewport";

// The reveal-side quiet window must span MORE than one pipeline coalesce
// batch (≥16 ms each): a TUI answering SIGWINCH often paces its redraw in
// RPC-round-trip writes ~20 ms apart, and a window that small fires between
// chunks and reveals the half-redrawn screen. Two batches plus slack (40 ms)
// is what the old value absorbed; going below it re-exposes partial frames.
const QUIET_MS = 40;
const MAX_PREVIEW_IDLE_MS = 5000;
const REDRAW_START_WAIT_MS = 500;
/**
 * A window resize fits every visible pane in the same frame, but each pane's
 * resize round trip and CLI redraw finish at a different time. Revealing them
 * one by one reads as a staggered "clunk-clunk". Previews that begin within
 * this window form one group and are uncovered together.
 */
const GROUP_JOIN_MS = 120;
/** Once one member is ready, wait at most this long for its siblings. */
const GROUP_REVEAL_CAP_MS = 120;
/**
 * After the journal applies a real grid change, the PTY has just sent
 * SIGWINCH. Until the program answers, the screen is only xterm's local
 * reflow of the old layout — revealing it shows an intermediate "wrong size"
 * frame that the program's redraw replaces a moment later (a visible jitter).
 * Wait this long for the answer to start; a silent program is revealed then.
 */
// First-ever wait for a program's SIGWINCH answer, and the ceiling for
// learned waits. A program that stayed silent last time gets the floor —
// waiting long for a program that never answers only delays the reveal.
const WINCH_RESPONSE_MS = 90;
const WINCH_RESPONSE_MIN_MS = 30;
/** First silence keeps a cautious budget; repeated silence drops to the floor. */
const SILENT_WINCH_FIRST_MS = 30;
const SILENT_WINCH_MS = 12;

/** Per-terminal SIGWINCH answer history: how long the last answer took (ms). */
interface WinchHistory {
  /** null when the deadline expired — the program did not answer. */
  lastLatency: number | null;
  /** Consecutive expired deadlines (any answer resets it). */
  silentStreak: number;
}
// Keyed by terminal so a pane learns its own program (a plain shell at the
// prompt never redraws; a TUI answers within a few ms).
const winchHistory = new WeakMap<object, WinchHistory>();

/** Wait for this terminal's program to answer SIGWINCH, adapted to history. */
function winchWaitMs(terminal: object): number {
  const history = winchHistory.get(terminal);
  if (!history) return WINCH_RESPONSE_MS;
  if (history.lastLatency === null) {
    return history.silentStreak >= 2 ? SILENT_WINCH_MS : SILENT_WINCH_FIRST_MS;
  }
  return Math.min(
    WINCH_RESPONSE_MS,
    Math.max(WINCH_RESPONSE_MIN_MS, Math.round(history.lastLatency * 2)),
  );
}

/**
 * A synchronized-output frame (DECSET 2026) just ran — the program is a TUI
 * that redraws in frames, so any learned "it never answers SIGWINCH" is stale
 * (that was an earlier plain shell in this pane). Drop the silent conclusion
 * so the next resize gets a fresh, safe budget. Latency learning is kept: a
 * TUI answering quickly is exactly what it measured.
 */
function resetSilentWinchLearning(terminal: object): void {
  const history = winchHistory.get(terminal);
  if (history && history.silentStreak >= 1) winchHistory.delete(terminal);
}
/** Output this soon after a reveal means a redraw landed on the uncovered screen. */
const LATE_OUTPUT_MS = 300;
/** Recent resize traces kept on `window.__resizeTrace` (devtools only). */
const TRACE_LIMIT = 50;
// Registry entries and pipelines can survive separate React Fast Refresh
// boundaries. Keep their transaction lookup shared across module replacements.
const previews: WeakMap<object, ZoomPreview> = import.meta.hot?.data
  ? (import.meta.hot.data.previews ??= new WeakMap<object, ZoomPreview>())
  : new WeakMap<object, ZoomPreview>();
const historyRebuilders: WeakSet<object> = import.meta.hot?.data
  ? (import.meta.hot.data.historyRebuilders ??= new WeakSet<object>())
  : new WeakSet<object>();

/** Learn the CLI's redraw protocol before the first zoom, including replay. */
export function observeHistoryRedraws(terminal: object): () => void {
  const parser = (terminal as Terminal).parser;
  if (!parser?.registerCsiHandler) return () => {};
  let clearedHistory = false;
  let usesSynchronizedOutput = false;
  // A shell's `clear` also erases history. Require the synchronized rendering
  // protocol as well, so clearing a shell does not delay its next zoom.
  const subscriptions = [
    parser.registerCsiHandler({ final: "J" }, (params) => {
      clearedHistory ||= params[0] === 3;
      if (clearedHistory && usesSynchronizedOutput) historyRebuilders.add(terminal);
      return false;
    }),
    parser.registerCsiHandler({ prefix: "?", final: "h" }, (params) => {
      usesSynchronizedOutput ||= params.includes(2026);
      if (params.includes(2026)) resetSilentWinchLearning(terminal);
      if (clearedHistory && usesSynchronizedOutput) historyRebuilders.add(terminal);
      return false;
    }),
  ];
  return () => { for (const subscription of subscriptions) subscription.dispose(); };
}

/**
 * A screen snapshot restores the CLI's screen without the clear/synchronized
 * sequences that taught `observeHistoryRedraws` its protocol. Persist the
 * learned flag with the snapshot and re-apply it on restore.
 */
export function isHistoryRebuilder(terminal: object): boolean {
  return historyRebuilders.has(terminal);
}

export function markHistoryRebuilder(terminal: object): void {
  historyRebuilders.add(terminal);
}

/** Stage timestamps of one resize transaction, relative to its first fit. */
export interface ResizeTrace {
  cols: number | null;
  rows: number | null;
  /** RPC answered by the daemon (ms since begin). */
  rpcAck?: number;
  /** The journal resize record reached xterm (ms since begin). */
  applied?: number;
  /** The CLI started its history rebuild (ED 3). */
  redrawStart?: number;
  /** The final grid was painted behind the cover. */
  ready?: number;
  /** The cover was removed. */
  revealed?: number;
  /** The program's first output after SIGWINCH (ms since begin). */
  winchResponse?: number;
  /** Output that arrived after the reveal (ms after reveal) — a visible second change. */
  lateOutput?: number;
  /** Cost of the cover snapshot (ms) — DOM-renderer panes clone every glyph. */
  captureMs?: number;
  /** Panes uncovered together with this one. */
  group: number;
  /** How the cover ended: painted, user input, idle timeout, cancel. */
  outcome?: "painted" | "dismissed";
}

const now = (): number => (typeof performance !== "undefined" ? performance.now() : Date.now());

function recordTrace(trace: ResizeTrace): void {
  const view = typeof window === "undefined" ? undefined : (window as unknown as { __resizeTrace?: ResizeTrace[] });
  if (view) {
    const list = (view.__resizeTrace ??= []);
    list.push(trace);
    if (list.length > TRACE_LIMIT) list.shift();
  }
  if (import.meta.env?.DEV) console.debug("[resize-trace]", trace);
}

/**
 * Previews begun by one window resize (or one font zoom across panes). Each
 * member reports when its final grid is painted; the group uncovers all ready
 * members in the same task — hence the same frame — once every member is
 * ready, or GROUP_REVEAL_CAP_MS after the first one became ready.
 */
class RevealGroup {
  readonly openedAt = Date.now();
  private readonly members = new Set<ZoomPreview>();
  private capTimer: ReturnType<typeof setTimeout> | null = null;
  private released = false;

  get isReleased(): boolean {
    return this.released;
  }

  get size(): number {
    return this.members.size;
  }

  add(member: ZoomPreview): void {
    this.members.add(member);
  }

  leave(member: ZoomPreview): void {
    this.members.delete(member);
    this.check();
  }

  check(): void {
    if (this.released) return;
    const live = [...this.members].filter((member) => member.connected);
    if (live.length === 0) {
      this.release();
      return;
    }
    const ready = live.filter((member) => member.awaitingGroup);
    if (ready.length === live.length) {
      this.release();
      return;
    }
    if (ready.length > 0 && this.capTimer === null) {
      this.capTimer = setTimeout(() => {
        this.capTimer = null;
        this.release();
      }, GROUP_REVEAL_CAP_MS);
    }
  }

  private release(): void {
    if (this.released) return;
    this.released = true;
    if (this.capTimer !== null) clearTimeout(this.capTimer);
    this.capTimer = null;
    if (currentGroup === this) currentGroup = null;
    const members = [...this.members];
    this.members.clear();
    // Uncover in one synchronous pass so the panes switch in the same frame.
    for (const member of members) member.groupReleased(members.length);
  }
}

let currentGroup: RevealGroup | null = null;

function joinGroup(): RevealGroup {
  if (!currentGroup || currentGroup.isReleased || Date.now() - currentGroup.openedAt > GROUP_JOIN_MS) {
    currentGroup = new RevealGroup();
  }
  return currentGroup;
}

/** A presentation layer only: output parsing, journal order and ACKs keep running. */
export function beginZoomPreview(terminal: object): void {
  const existing = previews.get(terminal);
  if (existing) {
    try {
      existing.zoom();
    } catch {
      existing.dispose();
    }
    return;
  }
  const term = terminal as Terminal;
  if (!term.element?.isConnected || !term.options || typeof document === "undefined") return;
  try {
    const preview = new ZoomPreview(term);
    previews.set(terminal, preview);
    preview.zoom();
  } catch {
    // A missing/lost renderer must never prevent zoom or terminal input.
  }
}

export function zoomPreviewFor(terminal: object): ZoomPreview | undefined {
  return previews.get(terminal);
}

export function cancelZoomPreview(terminal: object): void {
  previews.get(terminal)?.dispose();
}

/**
 * A CLI can clear and rebuild hundreds of KB of history on SIGWINCH. xterm's
 * synchronized-output watchdog expires after 1s, so buffering a few PTY chunks
 * cannot prevent that history from scrolling past. Keep the current screen
 * unchanged until the new font, grid and redraw are ready to present together.
 */
class ZoomPreview {
  private readonly overlay: HTMLDivElement;
  private screen?: HTMLElement;
  private readonly subscriptions: Array<{ dispose(): void }> = [];
  private target: { cols: number; rows: number } | null = null;
  private resized = false;
  private busy = false;
  private synchronized: boolean;
  private frameForTarget = false;
  private completedFrame = false;
  private screenCleared = false;
  private paintPending = false;
  private redrawStarted = false;
  private waitingForRedraw = false;
  private redrawStartTimer: ReturnType<typeof setTimeout> | null = null;
  private quietTimer: ReturnType<typeof setTimeout> | null = null;
  private deadline: ReturnType<typeof setTimeout>;
  private disposed = false;
  private group: RevealGroup | null = null;
  /** Painted at the final grid; waiting for siblings before uncovering. */
  awaitingGroup = false;
  /** The group let go: reveal on this member's own next paint. */
  private released = false;
  private trace: ResizeTrace | null = null;
  private traceStart = 0;
  /** The last finished trace, watched briefly for output after the reveal. */
  private revealedTrace: { trace: ResizeTrace; at: number } | null = null;
  /** SIGWINCH sent with a changed grid; waiting for the program to answer. */
  private awaitingWinch = false;
  private winchTimer: ReturnType<typeof setTimeout> | null = null;
  private winchSignaledAt = 0;
  /** Last cover snapshot cost — copied into the trace when one starts. */
  private lastCaptureMs: number | null = null;

  constructor(private readonly terminal: Terminal) {
    this.synchronized = terminal.modes.synchronizedOutputMode;
    this.overlay = document.createElement("div");
    this.overlay.className = "terminal-zoom-preview";
    this.overlay.setAttribute("aria-hidden", "true");
    this.overlay.inert = true;
    const background = terminal.options.theme?.background ?? "#000";
    // xterm 6's visible scrollbar has z-index:11 and is outside xterm-screen.
    // Cover it too: its live thumb otherwise travels through the rebuilt history.
    this.overlay.style.cssText = `position:absolute;inset:0;overflow:hidden;pointer-events:none;z-index:12;background:${background}`;
    this.capture();
    // xterm's root is already position:relative. The overlay has no layout
    // footprint, so FitAddon and ResizeObserver still measure the real pane.
    // Observe the protocol, not the watchdog-mutated modes getter. A long
    // synchronized frame remains hidden even if xterm's 1s timeout fires.
    try {
      this.subscriptions.push(deferTerminalPaints(terminal,
        () => this.overlay.isConnected && !this.paintPending));
      for (const [final, enabled] of [["h", true], ["l", false]] as const) {
        this.subscriptions.push(terminal.parser.registerCsiHandler({ prefix: "?", final }, (params) => {
          if (params.includes(2026)) {
            resetSilentWinchLearning(this.terminal);
            if (enabled) {
              this.frameForTarget = this.resized;
              this.completedFrame = false;
            } else {
              this.completedFrame = this.synchronized && this.frameForTarget && this.resized;
              this.frameForTarget = false;
            }
            this.synchronized = enabled;
          }
          return false;
        }));
      }
      this.subscriptions.push(terminal.onRender(() => {
        // A completed frame may be followed by spinner frames indefinitely.
        // Reveal on its actual paint instead of waiting for output to stop.
        if (this.paintPending && this.resized && !this.busy && !this.synchronized &&
          (!this.waitingForRedraw || this.redrawStarted)) this.ready();
      }));
      this.subscriptions.push(terminal.parser.registerCsiHandler({ final: "J" }, (params) => {
        if (params[0] === 2 || params[0] === 3) {
          // Some CLIs finish several small frames before starting the history
          // rebuild. Catch its clear BEFORE xterm applies it, even if an earlier
          // frame or a quiet shell has already been presented at the new size.
          if (!this.overlay.isConnected) {
            try {
              this.capture();
              terminal.element!.appendChild(this.overlay);
            } catch {
              this.dispose();
              return false;
            }
          }
          // Erasing only the visible screen can itself be a preliminary
          // frame. Only ED 3 identifies the history rebuild whose end closes
          // this transaction; ED 2 still re-covers its partial paints.
          this.redrawStarted ||= params[0] === 3;
          if (params[0] === 3) this.mark("redrawStart");
          this.screenCleared = true;
          // A rebuild after the final paint: that paint no longer counts.
          this.unready();
          if (params[0] === 3 && historyRebuilders.has(terminal)) this.waitForRedrawStart();
          this.frameForTarget = this.synchronized && this.resized;
          this.completedFrame = false;
          this.cancelReveal();
          this.keepAlive();
        }
        return false;
      }));
    } catch (error) {
      for (const subscription of this.subscriptions) subscription.dispose();
      throw error;
    }
    // Let deliberate reading/typing take precedence over a pending redraw.
    const onPointer = () => this.dispose();
    const onKey = (event: KeyboardEvent) => {
      if (["Meta", "Control", "Alt", "Shift"].includes(event.key)) return;
      const zoom = (event.metaKey || event.ctrlKey) && (
        ["Equal", "Minus", "Digit0", "NumpadAdd", "NumpadSubtract", "Numpad0"].includes(event.code) ||
        ["=", "+", "-", "0"].includes(event.key)
      );
      if (!zoom) this.dispose();
    };
    const element = terminal.element!;
    element.addEventListener("wheel", onPointer, { passive: true });
    element.addEventListener("pointerdown", onPointer);
    element.addEventListener("keydown", onKey, true);
    this.subscriptions.push({ dispose: () => {
      element.removeEventListener("wheel", onPointer);
      element.removeEventListener("pointerdown", onPointer);
      element.removeEventListener("keydown", onKey, true);
    } });
    element.appendChild(this.overlay);
    this.deadline = setTimeout(() => this.dispose(), MAX_PREVIEW_IDLE_MS);
  }

  zoom(): void {
    if (!this.overlay.isConnected) {
      this.capture();
      this.terminal.element!.appendChild(this.overlay);
    }
    this.awaitingWinch = false;
    this.clearWinchTimer();
    this.startTransaction();
    this.cancelReveal();
    this.keepAlive();
    this.resized = false;
    this.target = null;
    this.frameForTarget = false;
    this.completedFrame = false;
    this.redrawStarted = false;
    this.screenCleared = this.synchronized;
    this.waitingForRedraw = historyRebuilders.has(this.terminal);
    this.clearRedrawStartTimer();
  }

  expectResize(cols: number, rows: number): void {
    if (this.target?.cols === cols && this.target.rows === rows) return;
    this.target = { cols, rows };
    if (this.trace) {
      this.trace.cols = cols;
      this.trace.rows = rows;
    }
    this.unready();
    this.frameForTarget = false;
    this.completedFrame = false;
    this.redrawStarted = false;
    this.screenCleared = this.synchronized;
    this.resized = this.terminal.cols === cols && this.terminal.rows === rows;
    // No grid change means no SIGWINCH response is expected.
    if (this.resized) this.waitingForRedraw = false;
    this.settle();
  }

  /** A previous RPC can still change the grid even when it currently matches. */
  holdForResize(cols: number, rows: number): void {
    if (!this.overlay.isConnected || !this.resized || this.target?.cols !== cols || this.target.rows !== rows) return;
    this.resized = false;
    this.waitingForRedraw = historyRebuilders.has(this.terminal);
    this.unready();
    this.cancelReveal();
  }

  /** The daemon answered the resize RPC (trace only). */
  resizeAcknowledged(): void {
    this.mark("rpcAck");
  }

  resizeApplied(cols: number, rows: number): void {
    this.resized = this.target?.cols === cols && this.target.rows === rows;
    if (this.resized) this.mark("applied");
    this.unready();
    // A frame already in flight was built for the previous grid. Its end
    // cannot certify that the CLI has finished drawing the requested size.
    this.frameForTarget = false;
    this.completedFrame = false;
    this.redrawStarted = false;
    // An older frame can still be rebuilding behind this resize. Its end
    // must not take the fast path for a quiet, already-reflowed shell.
    this.screenCleared = this.synchronized;
    if (this.resized && this.waitingForRedraw) {
      this.waitForRedrawStart();
    }
    this.settle();
  }

  /**
   * The journal applied a live resize that changed the grid, so the program
   * received SIGWINCH. Keep the cover until its redraw starts (or a short
   * deadline passes) instead of revealing xterm's interim reflow.
   */
  resizeSignaled(): void {
    if (!this.overlay.isConnected || !this.resized || this.disposed) return;
    this.awaitingWinch = true;
    this.clearWinchTimer();
    this.cancelReveal();
    this.winchSignaledAt = now();
    const waitMs = winchWaitMs(this.terminal);
    this.winchTimer = setTimeout(() => {
      this.winchTimer = null;
      this.awaitingWinch = false;
      // The program stayed silent: lengthen the streak so the next wait
      // steps down (first silence stays cautious, repeated silence drops).
      winchHistory.set(this.terminal, {
        lastLatency: null,
        silentStreak: (winchHistory.get(this.terminal)?.silentStreak ?? 0) + 1,
      });
      this.settle();
    }, waitMs);
  }

  private clearWinchTimer(): void {
    if (this.winchTimer !== null) clearTimeout(this.winchTimer);
    this.winchTimer = null;
  }

  outputPending(): void {
    this.busy = true;
    if (this.revealedTrace && this.revealedTrace.trace.lateOutput === undefined) {
      const after = now() - this.revealedTrace.at;
      if (after <= LATE_OUTPUT_MS) this.revealedTrace.trace.lateOutput = Math.round(after);
      this.revealedTrace = null;
    }
    if (this.awaitingWinch) {
      // The program is answering SIGWINCH. Its redraw may span several
      // writes, so reveal only after output has been quiet for QUIET_MS.
      this.awaitingWinch = false;
      this.clearWinchTimer();
      const latency = now() - this.winchSignaledAt;
      winchHistory.set(this.terminal, { lastLatency: latency, silentStreak: 0 });
      if (this.trace) this.trace.winchResponse = Math.round(now() - this.traceStart);
      this.screenCleared = true;
    }
    // After revealing a quiet shell, only watch for a late clear. Ordinary
    // output must not keep the observer alive or turn every partial update
    // into a full repaint and synchronous viewport layout.
    if (!this.overlay.isConnected) return;
    this.completedFrame = false;
    this.cancelReveal();
    this.keepAlive();
    // Preliminary status frames are progress toward the resize response,
    // not evidence that the CLI stopped rebuilding. Count the fallback from
    // the last activity so a slow renderer cannot expose one of those frames.
    if (this.waitingForRedraw && !this.redrawStarted) this.waitForRedrawStart();
  }

  outputIdle(): void {
    this.busy = false;
    this.settle();
  }

  private settle(): void {
    this.cancelReveal();
    if (!this.overlay.isConnected || !this.resized || this.busy || this.synchronized || this.disposed) return;
    if (this.awaitingWinch) return;
    // ED 3 and the synchronized frame can arrive in separate writes. A gap
    // after the clear is not a completed redraw, even if the queue is idle.
    if (this.waitingForRedraw && (!this.redrawStarted || !this.completedFrame)) return;
    if (this.completedFrame || !this.screenCleared) {
      // A normal shell has no history rebuild to wait for. Commit its real
      // font/reflow on the next paint, without an artificial quiet delay.
      this.preparePaint();
      return;
    }
    this.quietTimer = setTimeout(() => {
      this.quietTimer = null;
      // Paint the final grid before uncovering it. Any arriving output or
      // another zoom cancels both the quiet timer and this scheduled reveal.
      this.preparePaint();
    }, QUIET_MS);
  }

  private cancelReveal(): void {
    // Output after the ready paint: the covered pixels are no longer final.
    // Drop out of the group's ready set; this member's next paint re-reports.
    if (this.awaitingGroup) this.unready();
    this.paintPending = false;
    if (this.quietTimer !== null) clearTimeout(this.quietTimer);
    this.quietTimer = null;
  }

  private capture(): void {
    const captureStart = now();
    try {
      this.captureInner();
    } finally {
      this.lastCaptureMs = Math.round(now() - captureStart);
      if (this.trace) this.trace.captureMs = this.lastCaptureMs;
    }
  }

  private captureInner(): void {
    const terminal = this.terminal;
    const source = terminal.element!.querySelector<HTMLElement>(".xterm-screen");
    if (!source) throw new Error("terminal screen unavailable");
    // Copy the last rendered pixels, never force-render the current buffer:
    // a synchronized CLI frame may still be halfway through rebuilding it.
    this.screen = snapshotElement(source);
    // Scaling an old raster keeps old line wraps and invents an intermediate
    // layout. Hold its exact pixels/position, then reveal the real new grid.
    Object.assign(this.screen.style, {
      position: "absolute", left: "0", top: "0", bottom: "auto", transform: "none",
    });
    this.overlay.replaceChildren(this.screen);
    const scrollbar = terminal.element!.querySelector<HTMLElement>(".xterm-scrollable-element > .scrollbar.vertical");
    if (scrollbar) {
      const copy = snapshotScrollbar(scrollbar);
      copy.style.backgroundColor = terminal.options.theme?.background ?? "#000";
      this.overlay.appendChild(copy);
    }
  }

  /** Whether this member still covers a live terminal (a detached one never blocks a group). */
  get connected(): boolean {
    return !this.disposed && this.overlay.isConnected && !!this.terminal.element?.isConnected;
  }

  private startTransaction(): void {
    // Another zoom while still covered keeps the original start, so the trace
    // measures the whole time the user saw the old screen.
    if (!this.trace) {
      this.traceStart = now();
      this.trace = { cols: null, rows: null, group: 1, captureMs: this.lastCaptureMs ?? undefined };
    }
    this.awaitingGroup = false;
    this.released = false;
    const group = joinGroup();
    if (group !== this.group) {
      this.group?.leave(this);
      this.group = group;
      group.add(this);
    }
  }

  private mark(stage: "rpcAck" | "applied" | "redrawStart" | "ready" | "revealed"): void {
    if (this.trace) this.trace[stage] = Math.round(now() - this.traceStart);
  }

  private endTrace(outcome: "painted" | "dismissed"): void {
    const trace = this.trace;
    if (!trace) return;
    this.trace = null;
    trace.outcome = outcome;
    if (trace.revealed === undefined) trace.revealed = Math.round(now() - this.traceStart);
    // The same object stays in `__resizeTrace`; a late redraw fills `lateOutput`.
    this.revealedTrace = outcome === "painted" ? { trace, at: now() } : null;
    recordTrace(trace);
  }

  /** The final grid is painted behind the cover. Reveal now, or with the group. */
  private ready(): void {
    if (this.trace?.ready === undefined) this.mark("ready");
    const group = this.group;
    if (this.released || !group || group.isReleased || group.size <= 1) {
      this.finish();
      return;
    }
    this.awaitingGroup = true;
    group.check();
  }

  /** New grid, clear or redraw: an earlier ready paint is stale. */
  private unready(): void {
    if (!this.awaitingGroup) return;
    this.awaitingGroup = false;
    this.group?.check();
  }

  /** Called by the group in one pass for all members. */
  groupReleased(size: number): void {
    this.group = null;
    if (this.disposed) return;
    this.released = true;
    if (this.trace) this.trace.group = size;
    if (this.awaitingGroup) {
      this.awaitingGroup = false;
      // Pixels behind the cover are the final grid only while paintPending
      // survived (no output since). Otherwise the next settle → paint reveals.
      if (this.paintPending) this.finish();
    }
  }

  private finish(): void {
    this.mark("revealed");
    this.endTrace("painted");
    this.awaitingGroup = false;
    if (this.redrawStarted) {
      this.dispose();
      return;
    }
    // A frame end only commits that frame, not the entire resize response.
    // Show the completed screen immediately, but retain the clear observer
    // until the delayed rebuild ends, input resumes or the idle timeout fires.
    this.completedFrame = false;
    this.cancelReveal();
    this.removeOverlay();
  }

  private preparePaint(): void {
    // Changing the viewport can schedule another render. Do it while covered,
    // before the final paint, never inside onRender after its pixels are ready.
    try {
      syncViewport(this.terminal);
    } catch {
      // A detached/lost renderer must not prevent releasing the preview.
    }
    this.paintPending = true;
    this.terminal.refresh(0, this.terminal.rows - 1);
  }

  private removeOverlay(): void {
    this.overlay.remove();
    // While observing a possible delayed clear, retain only the lightweight
    // handlers. A new clear/zoom captures the current screen again.
    this.overlay.replaceChildren();
    this.screen = undefined;
  }

  private keepAlive(): void {
    // A slow but progressing redraw can take longer than a fixed watchdog.
    // Give up only when progress stops; user input also dismisses the preview.
    clearTimeout(this.deadline);
    this.deadline = setTimeout(() => this.dispose(), MAX_PREVIEW_IDLE_MS);
  }

  private clearRedrawStartTimer(): void {
    if (this.redrawStartTimer !== null) clearTimeout(this.redrawStartTimer);
    this.redrawStartTimer = null;
  }

  private waitForRedrawStart(): void {
    this.waitingForRedraw = true;
    this.clearRedrawStartTimer();
    // Bound only the wait for the CLI to START its synchronized redraw.
    // An active frame remains covered even after this deadline.
    this.redrawStartTimer = setTimeout(() => {
      this.redrawStartTimer = null;
      this.waitingForRedraw = false;
      this.settle();
    }, REDRAW_START_WAIT_MS);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.endTrace(this.trace?.ready === undefined ? "dismissed" : "painted");
    this.awaitingGroup = false;
    const group = this.group;
    this.group = null;
    group?.leave(this);
    this.cancelReveal();
    clearTimeout(this.deadline);
    this.clearRedrawStartTimer();
    this.clearWinchTimer();
    for (const subscription of this.subscriptions) subscription.dispose();
    this.removeOverlay();
    previews.delete(this.terminal);
  }
}

function snapshotScrollbar(source: HTMLElement): HTMLElement {
  const copy = snapshotElement(source);
  copy.className = "terminal-zoom-scrollbar";
  copy.style.zIndex = "1";
  // Keep the thumb at the same place along its track, independent of the
  // text's scale. The root can change height before the new row count arrives.
  copy.style.left = "auto";
  copy.style.right = "0";
  copy.style.top = "0";
  copy.style.bottom = "0";
  copy.style.height = "auto";
  const thumb = source.querySelector<HTMLElement>(".slider");
  const copiedThumb = copy.querySelector<HTMLElement>(".slider");
  if (thumb && copiedThumb) {
    const trackRect = source.getBoundingClientRect();
    const thumbRect = thumb.getBoundingClientRect();
    const travel = trackRect.height - thumbRect.height;
    if (travel > 0) {
      const progress = Math.max(0, Math.min(1, (thumbRect.top - trackRect.top) / travel));
      copiedThumb.style.top = `calc(${progress * 100}% - ${progress * thumbRect.height}px)`;
    }
  }
  return copy;
}

function snapshotElement(source: HTMLElement): HTMLElement {
  const copy = source.cloneNode(true) as HTMLElement;
  const originals = [source, ...source.querySelectorAll<HTMLElement>("*")];
  const copies = [copy, ...copy.querySelectorAll<HTMLElement>("*")];
  // Freeze DOM renderer metrics without copying its scoped <style> rules:
  // duplicate rules would override the live renderer after its font changes.
  const properties = ["display", "position", "width", "height", "top", "left", "right", "bottom",
    "color", "background-color", "font-family", "font-size", "font-weight", "font-style",
    "line-height", "letter-spacing", "white-space", "overflow", "opacity", "text-decoration"];
  for (let index = 0; index < originals.length; index++) {
    const original = originals[index];
    const clone = copies[index];
    if (clone.tagName === "STYLE") {
      clone.remove();
      continue;
    }
    const style = getComputedStyle(original);
    for (const property of properties) clone.style.setProperty(property, style.getPropertyValue(property));
    clone.style.transition = "none";
    clone.style.animation = "none";
    if (original instanceof HTMLCanvasElement && clone instanceof HTMLCanvasElement) {
      const context = clone.getContext("2d");
      if (!context) throw new Error("snapshot canvas unavailable");
      context.drawImage(original, 0, 0);
    }
  }
  return copy;
}
