/**
 * 버전 조회/발견 상태: fake SystemProbe로 pending/null/value 상태,
 * 거부(rejection) 경로, 2초 timeout, Windows shim interpreter 제안.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CliCandidate, SystemProbe } from "./probeTypes";
import {
  VERSION_QUERY_TIMEOUT_MS,
  candidatesForKind,
  candidateSummary,
  discoverClis,
  interpreterSuggestionFor,
  queryVersionWithTimeout,
  versionProbeText,
} from "./probe";

function fakeProbe(impl: Partial<SystemProbe> = {}): SystemProbe {
  return {
    listClis: impl.listClis ?? (async () => []),
    queryVersion: impl.queryVersion ?? (async () => null),
  };
}

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("queryVersionWithTimeout (04 §5: 2초 timeout, 값이 없으면 null)", () => {
  it("값을 돌려주면 그대로 전달", async () => {
    const probe = fakeProbe({ queryVersion: async () => "1.0.42" });
    await expect(queryVersionWithTimeout(probe, "C:\\cli\\agent.exe")).resolves.toBe("1.0.42");
  });

  it("null이면 '감지 안 됨' 상태(null)로만 표시할 수 있다", async () => {
    const probe = fakeProbe({ queryVersion: async () => null });
    await expect(queryVersionWithTimeout(probe, "C:\\cli\\agent.exe")).resolves.toBeNull();
    expect(versionProbeText({ status: "unavailable", value: null })).toBe("감지 안 됨");
  });

  it("거부(rejection)되면 null — 예외가 UI로 새어 나가지 않는다", async () => {
    const probe = fakeProbe({
      queryVersion: () => Promise.reject(new Error("spawn 실패")),
    });
    await expect(queryVersionWithTimeout(probe, "C:\\cli\\agent.exe")).resolves.toBeNull();
  });

  it("2초 안에 응답이 없으면 timeout으로 null", async () => {
    const probe = fakeProbe({
      queryVersion: () => new Promise<null>(() => undefined), // 영원히 대기
    });
    const pending = queryVersionWithTimeout(probe, "C:\\cli\\agent.exe");
    await vi.advanceTimersByTimeAsync(VERSION_QUERY_TIMEOUT_MS - 1);
    const settled = await Promise.race([pending.then(() => true), Promise.resolve(false)]);
    expect(settled).toBe(false); // 아직 timeout 전
    await vi.advanceTimersByTimeAsync(1);
    await expect(pending).resolves.toBeNull();
  });

  it("빈 프로그램은 조회하지 않고 null", async () => {
    const probe = fakeProbe({ queryVersion: async () => "9.9" });
    await expect(queryVersionWithTimeout(probe, "   ")).resolves.toBeNull();
  });

  it("상태 문구: 미확인/확인 중/감지됨/감지 안 됨 — 값을 지어내지 않는다", () => {
    expect(versionProbeText({ status: "idle", value: null })).toBe("버전 미확인");
    expect(versionProbeText({ status: "pending", value: null })).toBe("확인 중…");
    expect(versionProbeText({ status: "value", value: "2.1.0" })).toBe("감지됨: 2.1.0");
    expect(versionProbeText({ status: "unavailable", value: null })).toBe("감지 안 됨");
  });
});

describe("discoverClis / 후보 제안", () => {
  it("probe가 없으면 빈 결과(직접 입력만 가능)", async () => {
    await expect(discoverClis(null)).resolves.toEqual({ status: "ready", candidates: [], error: null });
  });

  it("후보를 kind별로 걸러 보여 준다", async () => {
    const candidates: CliCandidate[] = [
      { program: "C:\\npm\\claude.cmd", kind: "claude", resolvedTarget: "C:\\npm\\node_modules\\cli.js", installForm: "npm" },
      { program: "C:\\tools\\codex.exe", kind: "codex", resolvedTarget: null, installForm: "native" },
    ];
    const state = await discoverClis(fakeProbe({ listClis: async () => candidates }));
    expect(state.status).toBe("ready");
    expect(candidatesForKind(state.candidates, "claude")).toHaveLength(1);
    expect(candidateSummary(candidates[0])).toContain("C:\\npm\\claude.cmd");
    expect(candidateSummary(candidates[0])).toContain("설치: npm");
    expect(candidateSummary(candidates[0])).toContain("대상: C:\\npm\\node_modules\\cli.js");
  });

  it("listClis 거부 시 failed 상태(폼은 직접 입력으로 동작)", async () => {
    const state = await discoverClis(
      fakeProbe({ listClis: () => Promise.reject(new Error("probe 없음")) }),
    );
    expect(state.status).toBe("failed");
    expect(state.error).toContain("probe");
  });
});

describe("Windows shim → interpreter 형태 제안 (02 §3)", () => {
  it("shim → .js target이면 node.exe + 스크립트 prefix 제안", () => {
    const candidate: CliCandidate = {
      program: "C:\\Users\\me\\npm\\claude.cmd",
      kind: "claude",
      resolvedTarget: "C:\\Users\\me\\npm\\node_modules\\@anthropic-ai\\claude-code\\cli.js",
      installForm: "npm",
    };
    const suggestion = interpreterSuggestionFor(candidate, "windows");
    expect(suggestion).not.toBeNull();
    expect(suggestion?.scriptArgvPrefix).toEqual([
      "C:\\Users\\me\\npm\\node_modules\\@anthropic-ai\\claude-code\\cli.js",
    ]);
    expect(suggestion?.executableHint).toBe("node.exe");
    expect(suggestion?.reason).toContain("cmd");
  });

  it("shim → native exe target이면 interpreter 제안 없음(그 exe를 program으로)", () => {
    const candidate: CliCandidate = {
      program: "C:\\npm\\tool.cmd",
      kind: "custom",
      resolvedTarget: "C:\\npm\\tool.exe",
      installForm: "npm",
    };
    expect(interpreterSuggestionFor(candidate, "windows")).toBeNull();
  });

  it("Windows가 아니거나 shim이 아니면 제안 없음", () => {
    const shim: CliCandidate = {
      program: "C:\\npm\\claude.cmd",
      kind: "claude",
      resolvedTarget: "C:\\x\\cli.js",
      installForm: "npm",
    };
    expect(interpreterSuggestionFor(shim, "other")).toBeNull();
    const native: CliCandidate = {
      program: "C:\\tools\\codex.exe",
      kind: "codex",
      resolvedTarget: null,
      installForm: "native",
    };
    expect(interpreterSuggestionFor(native, "windows")).toBeNull();
  });
});
