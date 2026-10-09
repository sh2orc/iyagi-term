import { expect, test } from "@playwright/test";

for (const gpu of [false, true]) {
  test(`zoom retains the visible text and covers a long resize redraw (${gpu ? "WebGL" : "DOM"})`, async ({ page }, testInfo) => {
    await page.goto("/", { waitUntil: "load" });
    await page.exposeFunction("captureZoomPreview", () => page.screenshot({ path: testInfo.outputPath("zoom-preview.png") }));
    const result = await page.evaluate(async (gpu) => {
      // Use the application's real registry, pipeline, font metrics and renderer.
      const load = (path: string) => import(path);
      const { TerminalRegistry } = await load("/src/features/terminal/registry.ts");
      const { SessionPipeline } = await load("/src/features/terminal/pipeline.ts");
      const setup = await load("/src/features/terminal/xtermSetup.ts");
      const frames = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
      const wait = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));
      document.body.innerHTML = '<div id="zoom-host" style="width:800px;height:500px"></div>';
      let seq = 0;
      let pipeline: InstanceType<typeof SessionPipeline>;
      const event = (kind: string, data = "", cols?: number, rows?: number) => ({
        session_id: "zoom-session", epoch: "e1", seq: String(++seq), kind,
        data_b64: btoa(data), raw_len: data.length, cols, rows,
      });
      const client = {
        sessionAttach: async () => ({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 }),
        sessionAck: () => {},
        sessionInput: async () => ({}),
        sessionResize: async ({ cols, rows }: { cols: number; rows: number }) => {
          await wait(20); // A real resize arrives asynchronously via the journal.
          pipeline.handleOutput(event("resize", "", cols, rows));
          return { resize_id: "r", applied_seq: String(seq) };
        },
      };
      const registry = new TerminalRegistry({
        createTerminal: setup.createDefaultTerminalFactory({ platform: "darwin" }),
        createFitAddon: setup.createDefaultFitAddon,
        createRenderer: gpu ? setup.createWebglRenderer : undefined,
      });
      const entry = registry.acquire("zoom-view");
      const term = entry.terminal;
      pipeline = new SessionPipeline({ client, sessionId: "zoom-session", viewId: "zoom-view", terminal: term });
      await pipeline.attach();
      entry.setFitHandler(({ cols, rows }: { cols: number; rows: number }) => pipeline.requestResize(cols, rows));
      entry.mount(document.getElementById("zoom-host"));
      await wait(60);
      await new Promise<void>((resolve) => term.write(
        Array.from({ length: 600 }, (_, i) => `line ${i} ${"text ".repeat(30)}\r\n`).join(""), resolve,
      ));
      await frames();
      term.scrollToLine(420);
      await frames();
      const visibleLine = () => {
        const buffer = term.buffer.active;
        let line = buffer.viewportY;
        while (line > 0 && buffer.getLine(line)?.isWrapped) line--;
        return buffer.getLine(line)?.translateToString(true).match(/^line \d+/)?.[0];
      };
      const expectedLine = visibleLine();
      const visibleLines = [expectedLine];
      const immediateSizes: number[] = [];
      const interimGeometry: number[][] = [];
      const screenRect = () => (term.element.querySelector(".terminal-zoom-preview .xterm-screen") ??
        term.element.querySelector(".xterm-screen"))!.getBoundingClientRect();
      for (const size of [14, 18, 12, 20, 13]) {
        const before = screenRect();
        entry.setFontSize(size);
        const after = screenRect();
        interimGeometry.push([after.x - before.x, after.y - before.y, after.width - before.width, after.height - before.height]);
        immediateSizes.push(term.options.fontSize);
        await wait(60);
        await frames();
        visibleLines.push(visibleLine());
      }
      // Restoring the bottom keeps following the live prompt while zooming.
      term.scrollToBottom();
      await frames();
      entry.setFontSize(14);
      await wait(60);
      await frames();
      const followsBottom = term.buffer.active.baseY === term.buffer.active.viewportY;

      // Simulate a TUI rebuilding history in separate PTY chunks on SIGWINCH.
      const paints: string[] = [];
      const collectVisibleText = () => {
        if (document.querySelector(".terminal-zoom-preview")) return;
        const buffer = term.buffer.active;
        paints.push(Array.from({ length: term.rows }, (_, row) =>
          buffer.getLine(buffer.viewportY + row)?.translateToString(true) ?? "").join("\n"));
      };
      const painted = term.onRender(collectVisibleText);
      pipeline.handleOutput(event("resize", "", term.cols, term.rows));
      const chunks = [
        "\x1b[2J\x1b[HREDRAW_START\r\n",
        ...Array.from({ length: 6 }, (_, i) => `REDRAW_MIDDLE_${i}\r\n`),
        "REDRAW_COMPLETE",
      ];
      for (const chunk of chunks) {
        pipeline.handleOutput(event("output", chunk));
        await wait(5);
      }
      await wait(80);
      await frames();
      await wait(150);
      await frames();
      collectVisibleText(); // Removing the overlay does not itself emit onRender.
      painted.dispose();

      // Real CLI resize output spans hundreds of records and can exceed both
      // the 256 KiB transport credit and xterm's 1s synchronized-output timeout.
      // The old short-burst test did not exercise this failure mode.
      // A real CLI has already used ED 3 and synchronized output. Remember that
      // before zoom so preliminary status frames are not exposed as a new layout.
      await new Promise<void>((resolve) => term.write("\x1b[?2026h\x1b[3J" + "READY_FOR_ZOOM\r\n".repeat(120) + "\x1b[?2026l", resolve));
      await frames();
      const scrollable = term.element.querySelector(".xterm-scrollable-element")!;
      scrollable.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
      await wait(120); // Keep the actual scrollbar visible, including its fade-in.
      const liveScrollbar = scrollable.querySelector<HTMLElement>(".scrollbar.vertical")!;
      const thumbProgress = (bar: HTMLElement | null) => {
        const thumb = bar?.querySelector<HTMLElement>(".slider");
        if (!bar || !thumb) return -1;
        const trackRect = bar.getBoundingClientRect();
        const thumbRect = thumb.getBoundingClientRect();
        return (thumbRect.top - trackRect.top) / (trackRect.height - thumbRect.height);
      };
      const progressBefore = thumbProgress(liveScrollbar);
      entry.setFontSize(12);
      const preview = document.querySelector<HTMLElement>(".terminal-zoom-preview");
      const immediatelyVisible = !!preview && preview.style.visibility !== "hidden";
      const scrollbarSamples: Array<{ covered: boolean; progress: number }> = [];
      const sampleScrollbar = () => {
        const rect = liveScrollbar.getBoundingClientRect();
        const pointerEvents = preview!.style.pointerEvents;
        // Include the presentation layer in hit testing to check paint order.
        preview!.inert = false;
        preview!.style.pointerEvents = "auto";
        const front = document.elementFromPoint(rect.right - 2, rect.top + rect.height / 2);
        preview!.style.pointerEvents = pointerEvents;
        preview!.inert = true;
        scrollbarSamples.push({ covered: preview!.contains(front),
          progress: thumbProgress(preview!.querySelector(".terminal-zoom-scrollbar")) });
      };
      sampleScrollbar();
      const previewPixels = () => {
        const canvases = preview?.querySelectorAll("canvas");
        if (!canvases?.length) return preview?.textContent;
        // The first canvas is the normally empty link-hover layer; the glyphs
        // live in the main WebGL canvas that follows it.
        return [...canvases].map((canvas) => {
          const bytes = canvas.getContext("2d")!.getImageData(0, 0, canvas.width, canvas.height).data;
          let hash = 0;
          for (const byte of bytes) hash = (hash * 31 + byte) | 0;
          return hash;
        }).join(",");
      };
      let pixelsBefore = previewPixels();
      const visiblePartialFrames: string[] = [];
      let redrawPaints = 0;
      const longRender = term.onRender(() => {
        redrawPaints++;
        if (document.querySelector(".terminal-zoom-preview")) return;
        const buffer = term.buffer.active;
        const text = Array.from({ length: term.rows }, (_, row) =>
          buffer.getLine(buffer.viewportY + row)?.translateToString(true) ?? "").join("\n");
        if (text.includes("HISTORY_ROW") && !text.includes("LONG_REDRAW_COMPLETE")) visiblePartialFrames.push(text);
      });
      await wait(40);
      // Actual CLI journals contain several small completed frames, then a
      // delayed clear/history rebuild. A frame end is not a resize-end signal.
      for (let index = 0; index < 3; index++) {
        pipeline.handleOutput(event("output", `\x1b[?2026h\x1b[${term.rows};1Hstatus ${index}\x1b[?2026l`));
        await frames();
      }
      await wait(180); // Also exceed the old quiet-shell reveal timer.
      const preliminaryViewStable = document.querySelector(".terminal-zoom-preview") === preview && previewPixels() === pixelsBefore;
      const ackBefore = pipeline.ackedThrough;
      pipeline.handleOutput(event("output", "\x1b[H\x1b[2J\x1b[3J\x1b[H\x1b[?2026h"));
      await frames();
      const delayedClearCovered = document.querySelector(".terminal-zoom-preview") === preview;
      pixelsBefore = previewPixels(); // The last completed screen is now the snapshot.
      for (let index = 0; index < 300; index++) {
        pipeline.handleOutput(event("output", `HISTORY_ROW_${index} ${"colored output ".repeat(75)}\r\n`));
        if (index === 20) await (window as unknown as { captureZoomPreview(): Promise<void> }).captureZoomPreview();
        if (index === 100) entry.setFontSize(11); // Reuse the stable snapshot during key repeat.
        await wait(5);
        if (index % 50 === 49) sampleScrollbar();
      }
      const survivedWatchdog = document.querySelector(".terminal-zoom-preview") === preview;
      const stablePreview = previewPixels() === pixelsBefore;
      const ackAdvanced = pipeline.ackedThrough > ackBefore + 250;
      const unfinishedRedrawPaints = redrawPaints;
      pipeline.handleOutput(event("output", "LONG_REDRAW_COMPLETE\x1b[?2026l"));
      await frames();
      const oldFrameStillCovered = document.querySelector(".terminal-zoom-preview") === preview;
      // The previous frame spanned a second zoom. Fresh frames now use the
      // latest grid; keep animation active so a 100ms quiet timer cannot pass.
      const animationCoverage: boolean[] = [];
      for (let index = 0; index < 6; index++) {
        const clearLatestGrid = index === 0 ? "\x1b[2J\x1b[3J\x1b[H" + "FINAL_ROW\r\n".repeat(80) : "\r";
        pipeline.handleOutput(event("output", `\x1b[?2026h${clearLatestGrid}LONG_REDRAW_COMPLETE animation ${index}\x1b[?2026l`));
        await frames();
        animationCoverage.push(!!document.querySelector(".terminal-zoom-preview"));
      }
      const finallyGone = !document.querySelector(".terminal-zoom-preview");
      const finalScrollProgress = thumbProgress(liveScrollbar);
      longRender.dispose();
      const hasCanvas = !!document.querySelector("#zoom-host canvas");
      const markerCount = term.markers.length;
      pipeline.dispose();
      entry.dispose();
      return { expectedLine, visibleLines, immediateSizes, interimGeometry, preliminaryViewStable, followsBottom, paints, hasCanvas, markerCount,
        immediatelyVisible, survivedWatchdog, stablePreview, ackAdvanced, unfinishedRedrawPaints, oldFrameStillCovered, animationCoverage,
        finallyGone, visiblePartialFrames, pixelsBefore, progressBefore, scrollbarSamples, finalScrollProgress, delayedClearCovered };
    }, gpu);

    expect(result.expectedLine).toBe("line 210");
    expect(result.visibleLines).toEqual(Array(6).fill(result.expectedLine));
    expect(result.immediateSizes).toEqual([14, 18, 12, 20, 13]);
    expect(result.interimGeometry.flat().every((delta) => Math.abs(delta) < 1)).toBe(true);
    expect(result.preliminaryViewStable).toBe(true);
    expect(result.followsBottom).toBe(true);
    expect(result.hasCanvas).toBe(gpu);
    expect(result.markerCount).toBe(0);
    expect(result.immediatelyVisible).toBe(true);
    expect(result.delayedClearCovered).toBe(true);
    expect(result.progressBefore).toBeCloseTo(1, 2);
    expect(result.scrollbarSamples.every((sample) => sample.covered)).toBe(true);
    for (const sample of result.scrollbarSamples) expect(sample.progress).toBeCloseTo(result.progressBefore, 2);
    expect(result.finalScrollProgress).toBeCloseTo(1, 2);
    expect(result.survivedWatchdog).toBe(true);
    expect(result.stablePreview).toBe(true);
    expect(result.ackAdvanced).toBe(true);
    expect(result.unfinishedRedrawPaints).toBe(0);
    expect(result.oldFrameStillCovered).toBe(true);
    // xterm parses asynchronously; WebKit can finish the first write after
    // that iteration's RAFs. The next frame must reveal while output continues.
    expect(result.animationCoverage.slice(1)).toEqual(Array(5).fill(false));
    expect(result.finallyGone).toBe(true);
    expect(result.visiblePartialFrames).toEqual([]);
    expect(result.pixelsBefore).toBeTruthy();
    if (gpu) expect(result.pixelsBefore!.split(",").some((hash) => Number(hash) !== 0)).toBe(true);
    expect(result.paints.some((paint) => paint.includes("REDRAW_COMPLETE"))).toBe(true);
    expect(result.paints.filter((paint) => paint.includes("REDRAW_START") && !paint.includes("REDRAW_COMPLETE"))).toEqual([]);
  });
}
