/** 메모리 진단 수집·직렬화(W1-11 매트릭스 프론트 측). */
import { describe, expect, it } from "vitest";
import {
  collectMemoryDiagnostics,
  formatMemoryDiagnostics,
  type MemoryDiagnostics,
} from "./memoryDiagnostics";

describe("collectMemoryDiagnostics", () => {
  it("live/replaying pane만 liveTerminals에 센다", () => {
    const d = collectMemoryDiagnostics(
      {
        panes: {
          a: { leafId: "a", viewId: "v-a", sessionId: "s1", workloadId: null, title: "t1", cwd: null, phase: "live", error: null, usage: null, flowBlocked: false },
          b: { leafId: "b", viewId: "v-b", sessionId: "s2", workloadId: null, title: "t2", cwd: null, phase: "exited", error: null, usage: null, flowBlocked: false },
          c: { leafId: "c", viewId: "v-c", sessionId: "s3", workloadId: null, title: "t3", cwd: null, phase: "replaying", error: null, usage: null, flowBlocked: false },
        },
      },
      { scrollbackLines: 2000 },
    );
    expect(d.liveTerminals).toBe(2); // live + replaying
    expect(d.totalPanes).toBe(3);
    expect(d.scrollbackBudgetBytes).toBe(2 * 2000 * 120);
  });

  it("scrollbackLines를 prefs에서 읽는다", () => {
    const d = collectMemoryDiagnostics({ panes: {} }, { scrollbackLines: 5000 });
    expect(d.scrollbackLines).toBe(5000);
    expect(d.scrollbackBudgetBytes).toBe(0); // live 없음
  });
});

describe("formatMemoryDiagnostics", () => {
  it("클립보드용 텍스트에 핵심 값이 담긴다", () => {
    const d: MemoryDiagnostics = {
      liveTerminals: 3,
      totalPanes: 4,
      scrollbackLines: 2000,
      scrollbackBudgetBytes: 720000,
      panes: [{ leafId: "leaf-1", sessionId: "s", phase: "live", title: "zsh" }],
      jsHeap: null,
    };
    const text = formatMemoryDiagnostics(d);
    expect(text).toContain("live terminals: 3 / 4 panes");
    expect(text).toContain("scrollback: 2000 lines/pane");
    expect(text).toContain("not available");
  });
});
