/**
 * Codex hooks 얇은 어댑터 — 실제 판정은 hooksIntegration.test.ts가
 * 검증한다. 여기서는 이 모듈이 `cli: "codex"`로 올바르게 위임하는지,
 * Claude에는 없는 이벤트 목록(SessionStart/SessionEnd만, Notification
 * 없음)을 반영하는지만 확인한다.
 */

import { describe, expect, it } from "vitest";
import {
  addedLines,
  classifyHooksStatus,
  manualSnippet,
  type CodexHooksStatus,
} from "./codexHooksIntegration";

const status = (overrides: Partial<CodexHooksStatus> = {}): CodexHooksStatus => ({
  path: "/home/user/.codex/hooks.json",
  exists: true,
  current: "{}\n",
  proposed: null,
  managedEntries: 0,
  expectedEntries: 2,
  hookCommand: "iyagi-termd hook --agent codex",
  ...overrides,
});

describe("codexHooksIntegration.classifyHooksStatus", () => {
  it("미등록(0/2)이면 would-register", () => {
    const st = status({ managedEntries: 0, proposed: "…" });
    expect(classifyHooksStatus(st, { kind: "ready" })).toEqual({
      kind: "would-register",
      status: st,
    });
  });

  it("부분 등록(1/2)이면 partial", () => {
    const st = status({ managedEntries: 1, proposed: "…" });
    expect(classifyHooksStatus(st, { kind: "ready" }).kind).toBe("partial");
  });

  it("완전 등록(2/2)이면 registered", () => {
    const st = status({ managedEntries: 2, proposed: null });
    expect(classifyHooksStatus(st, { kind: "ready" }).kind).toBe("registered");
  });

  it("비-Tauri면 unavailable", () => {
    expect(classifyHooksStatus(null, { kind: "not-tauri" })).toEqual({
      kind: "unavailable",
      reason: "not-tauri",
    });
  });
});

describe("codexHooksIntegration.manualSnippet", () => {
  it("SessionStart·SessionEnd만 담고 Notification은 없다", () => {
    const parsed = JSON.parse(manualSnippet("iyagi-termd hook --agent codex"));
    expect(Object.keys(parsed.hooks).sort()).toEqual(["SessionEnd", "SessionStart"]);
    expect(parsed.hooks.Notification).toBeUndefined();
  });
});

describe("codexHooksIntegration.addedLines", () => {
  it("proposed가 없으면 빈 배열", () => {
    expect(addedLines(null)).toEqual([]);
  });

  it("SessionStart·SessionEnd 두 이벤트 키를 발췌한다", () => {
    const proposed = JSON.stringify(
      {
        hooks: {
          SessionStart: [
            { hooks: [{ type: "command", command: "iyagi-termd hook --agent codex", timeout: 10 }] },
          ],
          SessionEnd: [
            { hooks: [{ type: "command", command: "iyagi-termd hook --agent codex", timeout: 10 }] },
          ],
        },
      },
      null,
      2,
    );
    const lines = addedLines(proposed);
    expect(lines.some((l) => l.includes("SessionStart"))).toBe(true);
    expect(lines.some((l) => l.includes("SessionEnd"))).toBe(true);
  });
});
