import { expect, test } from "@playwright/test";

for (const gpu of [false, true]) {
  test(`opposite zoom keys keep obsolete journal sizes covered (${gpu ? "WebGL" : "DOM"})`, async ({ page }) => {
    await page.goto("/", { waitUntil: "load" });
    const result = await page.evaluate(async (gpu) => {
      const load = (path: string) => import(path);
      const { TerminalRegistry } = await load("/src/features/terminal/registry.ts");
      const { SessionPipeline } = await load("/src/features/terminal/pipeline.ts");
      const setup = await load("/src/features/terminal/xtermSetup.ts");
      const frames = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
      document.body.innerHTML = '<div id="host" style="width:800px;height:500px"></div>';
      let seq = 0;
      let pipeline: InstanceType<typeof SessionPipeline>;
      const pending: Array<() => void> = [];
      const client = {
        sessionAttach: async () => ({ epoch: "e1", replay_from_seq: "1", last_seq: "0", cols: 80, rows: 24 }),
        sessionAck: () => {}, sessionInput: async () => ({}),
        sessionResize: ({ cols, rows }: { cols: number; rows: number }) => new Promise((resolve) => {
          pending.push(() => {
            pipeline.handleOutput({ session_id: "s", epoch: "e1", seq: String(++seq), kind: "resize",
              data_b64: "", raw_len: 0, cols, rows });
            resolve({ resize_id: "r", applied_seq: String(seq) });
          });
        }),
      };
      const registry = new TerminalRegistry({
        createTerminal: setup.createDefaultTerminalFactory({ platform: "darwin" }),
        createFitAddon: setup.createDefaultFitAddon,
        createRenderer: gpu ? setup.createWebglRenderer : undefined,
      });
      const entry = registry.acquire("v");
      const term = entry.terminal;
      pipeline = new SessionPipeline({ client, sessionId: "s", viewId: "v", terminal: term });
      await pipeline.attach();
      entry.setFitHandler(({ cols, rows }: { cols: number; rows: number }) => pipeline.requestResize(cols, rows));
      entry.mount(document.getElementById("host"));
      term.options.cursorBlink = false;
      pending.shift()!(); // Initial fit.
      await frames();
      await new Promise<void>((resolve) => term.write("\x1b[?25l" + "ORIGINAL_SCREEN\r\n".repeat(100), resolve));
      term.scrollToLine(25);
      await frames();
      const start = { size: term.options.fontSize, cols: term.cols, rows: term.rows, viewport: term.buffer.active.viewportY };
      const covered = () => !!term.element.querySelector(".terminal-zoom-preview");
      const obsoletePaints: number[] = [];
      const listener = term.onRender(() => {
        if (!covered() && (term.cols !== start.cols || term.rows !== start.rows)) obsoletePaints.push(term.cols);
      });
      entry.setFontSize(start.size + 2);
      entry.setFontSize(start.size); // The old grid still happens to match, but a different one is in flight.
      await frames();
      const beforeJournal = covered();
      pending.shift()!(); // The obsolete size is mandatory journal history.
      await frames();
      const afterObsoleteJournal = covered();
      pending.shift()!(); // Finally back to the requested size.
      await frames();
      const afterFinalJournal = covered();
      const restored = { cols: term.cols, rows: term.rows, viewport: term.buffer.active.viewportY };
      listener.dispose();
      pipeline.dispose();
      entry.dispose();
      return { start, beforeJournal, afterObsoleteJournal, afterFinalJournal, restored, obsoletePaints };
    }, gpu);
    expect(result.beforeJournal).toBe(true);
    expect(result.afterObsoleteJournal).toBe(true);
    expect(result.afterFinalJournal).toBe(false);
    expect(result.obsoletePaints).toEqual([]);
    expect(result.restored).toEqual({ cols: result.start.cols, rows: result.start.rows, viewport: result.start.viewport });
  });
}
