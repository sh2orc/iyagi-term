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

  it("carries the repo-only fix too — the link layer no longer acquires its own 2048-capped atlas", () => {
    // 소스만 고치고 번들을 다시 묶지 않으면 이 수정만 빠진 채 배포된다(크기가 바뀔 때마다 아틀라스를
    // 버리고 다시 그린다). 링크 레이어의 `_refreshCharAtlas(terminal, colorSet)`가 그 흔적이다.
    expect(bundle).not.toContain("_refreshCharAtlas(terminal");
    expect(bundle).not.toMatch(/acquireTextureAtlas\([^)]*,\s*2048\)/);
  });

  it("carries the fractional-DPR fixes — exact css canvas size and integer atlas font size", () => {
    // 분수 DPR(Windows 125%/150%)에서만 글자가 옅게 번지던 두 원인(README "분수 DPR" 항목):
    // 1) CSS 캔버스 크기의 정수 반올림이 요소 박스와 셀 배수 백퍼를 어긋나게 해 글리프 피치가
    //    소수 디바이스 픽셀이 된다 — 정확한 몫(device / dpr)을 쓰는 흔적.
    // 2) 아틀라스가 fontSize × dpr px(예: 16.25px)로 글리프를 구워 힌팅이 안 걸리던 것 — Math.round.
    expect(bundle).toContain(
      "this.dimensions.css.canvas.width = this.dimensions.device.canvas.width / this._devicePixelRatio;",
    );
    expect(bundle).not.toContain(
      "Math.round(this.dimensions.device.canvas.width / this._devicePixelRatio)",
    );
    expect(bundle).toContain("Math.round(this._config.fontSize * this._config.devicePixelRatio)}px");
  });

  it("is what the terminal setup loads (not the npm package)", () => {
    const setup = readFileSync(new URL("../src/features/terminal/xtermSetup.ts", import.meta.url), "utf8");
    expect(setup).toContain('from "../../vendor/xterm-addon-webgl/addon-webgl"');
    expect(setup).not.toContain('from "@xterm/addon-webgl"');
  });
});
