/**
 * Claude Code hooks 얇은 어댑터 — 실제 판정은 hooksIntegration.test.ts가
 * 검증한다. 여기서는 이 모듈이 `cli: "claude"`로 올바르게 위임하는지,
 * 기존 공개 API(`ClaudeHooksStatus`/`classifyHooksStatus`/`manualSnippet`/
 * `addedLines`)가 그대로 유지되는지만 확인한다.
 */

import { describe, expect, it } from "vitest";
import {
  addedLines,
  classifyHooksStatus,
  manualSnippet,
  type ClaudeHooksStatus,
} from "./claudeHooksIntegration";

const status = (overrides: Partial<ClaudeHooksStatus> = {}): ClaudeHooksStatus => ({
  path: "/home/user/.claude/settings.json",
  exists: true,
  current: "{}\n",
  proposed: null,
  managedEntries: 0,
  expectedEntries: 3,
  hookCommand: "iyagi-termd hook",
  ...overrides,
});

describe("claudeHooksIntegration.classifyHooksStatus", () => {
  it("미등록(0/3)이면 would-register", () => {
    const st = status({ managedEntries: 0, proposed: "…" });
    expect(classifyHooksStatus(st, { kind: "ready" })).toEqual({
      kind: "would-register",
      status: st,
    });
  });

  it("부분 등록(1/3)이면 partial", () => {
    const st = status({ managedEntries: 1, proposed: "…" });
    expect(classifyHooksStatus(st, { kind: "ready" }).kind).toBe("partial");
  });

  it("완전 등록(3/3)이면 registered", () => {
    const st = status({ managedEntries: 3, proposed: null });
    expect(classifyHooksStatus(st, { kind: "ready" }).kind).toBe("registered");
  });

  it("비-Tauri면 unavailable", () => {
    expect(classifyHooksStatus(null, { kind: "not-tauri" })).toEqual({
      kind: "unavailable",
      reason: "not-tauri",
    });
  });
});

describe("claudeHooksIntegration.manualSnippet", () => {
  it("Notification·SessionStart·SessionEnd 세 이벤트를 모두 담는다", () => {
    const parsed = JSON.parse(manualSnippet("iyagi-termd hook"));
    expect(Object.keys(parsed.hooks).sort()).toEqual([
      "Notification",
      "SessionEnd",
      "SessionStart",
    ]);
  });
});

describe("claudeHooksIntegration.addedLines", () => {
  it("proposed가 없으면 빈 배열", () => {
    expect(addedLines(null)).toEqual([]);
  });

  it("세 이벤트 키를 모두 발췌한다", () => {
    const proposed = JSON.stringify(
      {
        hooks: {
          Notification: [{ hooks: [{ type: "command", command: "iyagi-termd hook", timeout: 10 }] }],
          SessionStart: [{ hooks: [{ type: "command", command: "iyagi-termd hook", timeout: 10 }] }],
          SessionEnd: [{ hooks: [{ type: "command", command: "iyagi-termd hook", timeout: 10 }] }],
        },
      },
      null,
      2,
    );
    const lines = addedLines(proposed);
    expect(lines.some((l) => l.includes("Notification"))).toBe(true);
    expect(lines.some((l) => l.includes("SessionStart"))).toBe(true);
    expect(lines.some((l) => l.includes("SessionEnd"))).toBe(true);
  });
});
