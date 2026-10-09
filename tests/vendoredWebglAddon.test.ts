import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { WebglAddon } from "../src/vendor/xterm-addon-webgl/addon-webgl";

// 앱은 npm의 @xterm/addon-webgl 대신 vendor/xterm-addon-webgl(아틀라스 병합·공유 수정)을 묶은 번들을 쓴다.
// 번들이 패치된 소스에서 다시 묶였는지(수정의 흔적이 있는지)와, 앱이 그 번들을 쓰는지를 지킨다.
describe("vendored WebGL addon (vendor/xterm-addon-webgl)", () => {
  const bundle = readFileSync(new URL("../src/vendor/xterm-addon-webgl/addon-webgl.js", import.meta.url), "utf8");

  it("exports the addon class", () => {
    expect(typeof WebglAddon).toBe("function");
  });

  it("is built from the patched sources — per-renderer page layout versions, merge retries, eviction, no mipmaps", () => {
    expect(bundle).toContain("pageLayoutVersion");
    expect(bundle).toContain("_lastSeenPageLayoutVersion");
    expect(bundle).toContain("invalidateAtlasTextures");
    expect(bundle).toContain("_evictAllPages");
    expect(bundle).toContain("AtlasPage.nextVersion");
    expect(bundle).not.toContain("_requestClearModel");
    expect(bundle).not.toContain("generateMipmap");
  });

  it("is what the terminal setup loads (not the npm package)", () => {
    const setup = readFileSync(new URL("../src/features/terminal/xtermSetup.ts", import.meta.url), "utf8");
    expect(setup).toContain('from "../../vendor/xterm-addon-webgl/addon-webgl"');
    expect(setup).not.toContain('from "@xterm/addon-webgl"');
  });
});
