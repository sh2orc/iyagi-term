import { expect, test } from "@playwright/test";

for (const gpu of [false, true]) {
  test(`resize captures the last displayed frame while a CLI frame is unfinished (${gpu ? "WebGL" : "DOM"})`, async ({ page }) => {
    await page.goto("/", { waitUntil: "load" });
    const result = await page.evaluate(async (gpu) => {
      const load = (path: string) => import(path);
      const { TerminalRegistry } = await load("/src/features/terminal/registry.ts");
      const { beginZoomPreview, cancelZoomPreview } = await load("/src/features/terminal/zoomPreview.ts");
      const setup = await load("/src/features/terminal/xtermSetup.ts");
      document.body.innerHTML = '<div id="host" style="width:800px;height:500px"></div>';
      const registry = new TerminalRegistry({
        createTerminal: setup.createDefaultTerminalFactory({ platform: "darwin" }),
        createRenderer: gpu ? setup.createWebglRenderer : undefined,
      });
      const entry = registry.acquire("snapshot");
      const term = entry.terminal;
      entry.mount(document.getElementById("host"));
      term.options.cursorBlink = false;
      const write = (text: string) => new Promise<void>((resolve) => term.write(text, resolve));
      const frames = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
      const fingerprint = (root: HTMLElement) => {
        const canvases = [...root.querySelectorAll<HTMLCanvasElement>("canvas")];
        if (!canvases.length) return root.querySelector(".xterm-rows")?.textContent;
        return canvases.map((source) => {
          const copy = document.createElement("canvas");
          copy.width = source.width;
          copy.height = source.height;
          const ctx = copy.getContext("2d")!;
          ctx.drawImage(source, 0, 0);
          let hash = 0;
          for (const byte of ctx.getImageData(0, 0, copy.width, copy.height).data) hash = (hash * 31 + byte) | 0;
          return hash;
        }).join(",");
      };
      await write("\x1b[?25l" + "LAST_DISPLAYED_FRAME\r\n".repeat(12));
      await frames();
      const displayed = fingerprint(term.element.querySelector(".xterm-screen"));
      await write("\x1b[?2026h\x1b[2J\x1b[HUNFINISHED_CLI_FRAME");
      await frames();
      const bufferHasPendingFrame = term.buffer.active.getLine(term.buffer.active.baseY)?.translateToString(true).includes("UNFINISHED_CLI_FRAME");
      const synchronized = term.modes.synchronizedOutputMode;
      beginZoomPreview(term);
      const snapshot = fingerprint(term.element.querySelector(".terminal-zoom-preview .xterm-screen"));
      cancelZoomPreview(term);
      await write("\x1b[?2026l");
      entry.dispose();
      return { displayed, snapshot, bufferHasPendingFrame, synchronized };
    }, gpu);
    expect(result.bufferHasPendingFrame).toBe(true);
    expect(result.synchronized).toBe(true);
    expect(result.displayed).toBeTruthy();
    if (gpu) expect(result.displayed!.split(",").some((hash: string) => Number(hash) !== 0)).toBe(true);
    expect(result.snapshot).toEqual(result.displayed);
  });
}
