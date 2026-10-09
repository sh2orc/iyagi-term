import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { App } from "./App";
import { t } from "../i18n";

describe("workbench shell", () => {
  beforeEach(() => vi.stubGlobal("navigator", { language: "ko-KR", languages: ["ko-KR"], userAgent: "test", platform: "MacIntel" }));
  afterEach(() => vi.unstubAllGlobals());
  it("renders top bar, empty project state, and the resource strip", () => {
    const html = renderToString(<App />);
    expect(html).toContain("top-bar");
    expect(html).toContain(t("terminal.newTerminal"));
    expect(html).toContain(t("settings.title"));
    expect(html).toContain("빈 프로젝트");
    expect(html).toContain("resource-strip");
    // 측정 전 상태도 "—"로 표시된다(U15).
    expect(html).toContain("—");
  });
});
