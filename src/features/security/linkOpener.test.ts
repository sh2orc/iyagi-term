import { describe, expect, it } from "vitest";
import { openExternal, sanitizeUrl } from "./linkOpener";

describe("openExternal", () => {
  it("Tauri 웹뷰에서는 shell 플러그인 open 명령을 invoke로 부른다", async () => {
    const calls: Array<[string, Record<string, unknown> | undefined]> = [];
    const ok = await openExternal("https://example.com/a?b=1", {
      isTauri: () => true,
      invoke: async (command, args) => {
        calls.push([command, args]);
      },
    });
    expect(ok).toBe(true);
    expect(calls).toEqual([["plugin:shell|open", { path: "https://example.com/a?b=1" }]]);
  });

  it("허용 scheme 밖이면 아무것도 부르지 않는다", async () => {
    let invoked = 0;
    const ok = await openExternal("file:///etc/passwd", {
      isTauri: () => true,
      invoke: async () => {
        invoked += 1;
      },
    });
    expect(ok).toBe(false);
    expect(invoked).toBe(0);
    expect(sanitizeUrl("javascript:alert(1)")).toBeNull();
  });

  it("invoke가 실패하면 false — 웹뷰는 window.open 폴백이 없다", async () => {
    const ok = await openExternal("https://example.com", {
      isTauri: () => true,
      invoke: async () => {
        throw new Error("scope denied");
      },
    });
    expect(ok).toBe(false);
  });
});
