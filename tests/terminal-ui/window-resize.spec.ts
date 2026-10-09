import { expect, test } from "@playwright/test";

for (const gpu of [false, true]) {
  test(`window shrink and maximize keeps every pane fitted and hides partial redraws (${gpu ? "WebGL" : "DOM"})`, async ({ page }) => {
    await page.goto("/", { waitUntil: "load" });
    await page.exposeFunction("resizeTestWindow", (width: number, height: number) => page.setViewportSize({ width, height }));
    const result = await page.evaluate(async (gpu) => {
      const load = (path: string) => import(path);
      const { TerminalRegistry } = await load("/src/features/terminal/registry.ts");
      const { SessionPipeline } = await load("/src/features/terminal/pipeline.ts");
      const setup = await load("/src/features/terminal/xtermSetup.ts");
      const wait = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
      const frames = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
      const resizeWindow = (window as unknown as { resizeTestWindow(w: number, h: number): Promise<void> }).resizeTestWindow;
      document.body.innerHTML = '<div style="display:flex;width:100vw;height:100vh"><div id="left" style="flex:1;min-width:0;overflow:hidden"></div><div id="right" style="flex:1;min-width:0;overflow:hidden"></div></div>';
      document.body.style.margin = "0";
      const registry = new TerminalRegistry({
        createTerminal: setup.createDefaultTerminalFactory({ platform: "darwin" }),
        createFitAddon: setup.createDefaultFitAddon,
        createRenderer: gpu ? setup.createWebglRenderer : undefined,
        createResizeObserver: setup.browserResizeObserverFactory,
      });
      const panes = await Promise.all(["left", "right"].map(async (id) => {
        let seq = 0;
        let pipeline: InstanceType<typeof SessionPipeline>;
        let rebuildingCli = false;
        let preliminaryTimer: ReturnType<typeof setInterval> | null = null;
        const stopPreliminaryFrames = () => {
          if (preliminaryTimer !== null) clearInterval(preliminaryTimer);
          preliminaryTimer = null;
        };
        const event = (kind: string, data = "", cols?: number, rows?: number) => ({
          session_id: id, epoch: "e1", seq: String(++seq), kind,
          data_b64: btoa(data), raw_len: data.length, cols, rows,
        });
        const client = {
          sessionAttach: async () => ({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 }),
          sessionAck: () => {},
          sessionInput: async () => ({}),
          sessionResize: async ({ cols, rows }: { cols: number; rows: number }) => {
            await wait(20);
            pipeline.handleOutput(event("resize", "", cols, rows));
            if (rebuildingCli) {
              stopPreliminaryFrames();
              // The CLI responds to SIGWINCH independently of Playwright's
              // setViewportSize completion (which can await slow GPU work).
              // Keep sending preliminary frames until the history clear below.
              preliminaryTimer = setInterval(() => {
                pipeline.handleOutput(event("output", `\x1b[?2026h\x1b[${rows};1Hstatus\x1b[?2026l`));
              }, 30);
            }
            return { resize_id: "r", applied_seq: String(seq) };
          },
        };
        const entry = registry.acquire(id);
        const term = entry.terminal;
        pipeline = new SessionPipeline({ client, sessionId: id, viewId: id, terminal: term });
        await pipeline.attach();
        entry.setFitHandler(({ cols, rows }: { cols: number; rows: number }) => pipeline.requestResize(cols, rows));
        entry.mount(document.getElementById(id));
        term.element.style.height = "100%";
        return { id, entry, term, pipeline, event, stopPreliminaryFrames,
          startCli: () => { rebuildingCli = true; } };
      }));
      await wait(80);
      for (const { term } of panes) {
        await new Promise<void>((resolve) => term.write(Array.from({ length: 600 }, (_, i) => `line ${i} ${"text ".repeat(25)}\r\n`).join(""), resolve));
        term.scrollToLine(400);
      }
      await frames();
      const visibleLine = (term: typeof panes[number]["term"]) => {
        const buffer = term.buffer.active;
        let row = buffer.viewportY;
        while (row > 0 && buffer.getLine(row)?.isWrapped) row--;
        return buffer.getLine(row)?.translateToString(true).match(/^line \d+/)?.[0];
      };
      const anchors = panes.map(({ term }) => visibleLine(term));
      const awaitPresentation = async () => {
        // Software WebGL can take several frames to paint both panes. Wait
        // for that paint, staying below the old 500ms CLI-redraw deadline.
        for (let frame = 0; frame < 6; frame++) {
          await frames();
          if (panes.every(({ term }) => !term.element.querySelector(".terminal-zoom-preview"))) return;
        }
      };
      const anchorSamples: Array<Array<string | undefined>> = [];
      const fitted: boolean[] = [];
      const fitDetails: unknown[] = [];
      for (const [width, height] of [[720, 480], [1440, 900], [800, 500], [1440, 900]]) {
        await resizeWindow(width, height);
        await wait(100);
        await awaitPresentation();
        anchorSamples.push(panes.map(({ term }) => visibleLine(term)));
        for (const { id, term } of panes) {
          const host = document.getElementById(id)!.getBoundingClientRect();
          const screen = term.element.querySelector(".xterm-screen")!.getBoundingClientRect();
          const cellWidth = screen.width / term.cols;
          const cellHeight = screen.height / term.rows;
          fitted.push(host.width - screen.width >= 0 && host.width - screen.width < cellWidth + 20 &&
            host.height - screen.height >= 0 && host.height - screen.height < cellHeight + 1);
          fitDetails.push({ id, host: [host.width, host.height], screen: [screen.width, screen.height], grid: [term.cols, term.rows] });
        }
      }
      // Learn a CLI's initial synchronized render before shrinking the window.
      for (const { term, startCli } of panes) {
        startCli();
        term.scrollToBottom();
        await new Promise<void>((resolve) => term.write("\x1b[?2026h\x1b[3J" + "READY\r\n".repeat(120) + "\x1b[?2026l", resolve));
      }
      await frames();
      const partialPaints: string[] = [];
      const listeners = panes.map(({ id, term }) => term.onRender(() => {
        if (term.element.querySelector(".terminal-zoom-preview")) return;
        const buffer = term.buffer.active;
        const visible = Array.from({ length: term.rows }, (_, row) => buffer.getLine(buffer.viewportY + row)?.translateToString(true) ?? "").join("\n");
        if (visible.includes("REBUILD_") && !visible.includes("RESIZE_COMPLETE")) partialPaints.push(id);
      }));
      const coverage: boolean[] = [];
      const heldUntilRedraw: boolean[] = [];
      for (const [width, height] of [[720, 480], [1440, 900]]) {
        await resizeWindow(width, height);
        await wait(60);
        await frames();
        const snapshots = panes.map(({ term }) => term.element.querySelector(".terminal-zoom-preview"));
        // A reflowed old screen is an intermediate layout for a rebuilding
        // CLI. Small completed status frames must not expose it either.
        for (let index = 0; index < 3; index++) {
          for (const pane of panes) pane.pipeline.handleOutput(pane.event("output", `\x1b[?2026h\x1b[${pane.term.rows};1Hstatus ${index}\x1b[?2026l`));
          await frames();
          heldUntilRedraw.push(...panes.map(({ term }, i) => !!snapshots[i] && term.element.querySelector(".terminal-zoom-preview") === snapshots[i]));
        }
        // Real CLI journals clear history in a separate write before starting
        // synchronized output, sometimes waiting for a cursor-position reply.
        for (const pane of panes) {
          pane.stopPreliminaryFrames();
          pane.pipeline.handleOutput(pane.event("output", "\x1b[2J\x1b[3J\x1b[H"));
        }
        await wait(200);
        coverage.push(...panes.map(({ term }) => !!term.element.querySelector(".terminal-zoom-preview")));
        for (const pane of panes) pane.pipeline.handleOutput(pane.event("output", "\x1b[?2026h"));
        // Exceed xterm's synchronized-output watchdog during each redraw.
        for (let row = 0; row < 100; row++) {
          for (const pane of panes) pane.pipeline.handleOutput(pane.event("output", `REBUILD_${row} ${"history ".repeat(30)}\r\n`));
          await wait(12);
        }
        coverage.push(...panes.map(({ term }) => !!term.element.querySelector(".terminal-zoom-preview")));
        for (const pane of panes) pane.pipeline.handleOutput(pane.event("output", "RESIZE_COMPLETE\x1b[?2026l"));
        await wait(60);
        await frames();
      }
      const released = panes.every(({ term }) => !term.element.querySelector(".terminal-zoom-preview"));
      const followsBottom = panes.every(({ term }) => term.buffer.active.baseY === term.buffer.active.viewportY);
      for (const listener of listeners) listener.dispose();
      for (const { pipeline, entry } of panes) { pipeline.dispose(); entry.dispose(); }
      return { anchors, anchorSamples, fitted, fitDetails, coverage, heldUntilRedraw, partialPaints, released, followsBottom };
    }, gpu);
    expect(result.anchors.every(Boolean)).toBe(true);
    for (const sample of result.anchorSamples) expect(sample, JSON.stringify(result)).toEqual(result.anchors);
    expect(result.fitted.every(Boolean), JSON.stringify(result)).toBe(true);
    expect(result.heldUntilRedraw.every(Boolean), JSON.stringify(result)).toBe(true);
    expect(result.partialPaints).toEqual([]);
    expect(result.coverage.every(Boolean)).toBe(true);
    expect(result.released).toBe(true);
    expect(result.followsBottom).toBe(true);
  });
}
