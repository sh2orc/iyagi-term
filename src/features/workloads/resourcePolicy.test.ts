/**
 * 자원 가드 정책 규칙(resourcePolicy.ts): 데몬과 같은 범위 검증, 저장값 정리, 전체
 * 정책 합성, "바꾼 것만" 남기는 변경분 갱신, 화면 단위 변환, 호스트 기본값 유도.
 */

import { describe, expect, it } from "vitest";
import type { GuardPolicy } from "../../generated/GuardPolicy";
import {
  FALLBACK_GUARD_POLICY,
  GUARD_RSS_MAX_BYTES,
  GUARD_RSS_MIN_BYTES,
  bytesToGib,
  gibToBytes,
  guardFieldError,
  guardPolicyProblem,
  hostDerivedGuardPolicy,
  msToSeconds,
  nextGuardOverrides,
  overlayGuardPolicy,
  sameGuardPolicy,
  sanitizeGuardOverrides,
  secondsToMs,
} from "./resourcePolicy";

const GiB = 1024 ** 3;

describe("guardFieldError — 데몬과 같은 범위", () => {
  it("cpu는 1..=1024의 정수", () => {
    expect(guardFieldError("cpu_cores_limit", 1)).toBeNull();
    expect(guardFieldError("cpu_cores_limit", 1024)).toBeNull();
    expect(guardFieldError("cpu_cores_limit", 0)).toBe("settings.resources.error.cpuCores");
    expect(guardFieldError("cpu_cores_limit", 1025)).toBe("settings.resources.error.cpuCores");
    expect(guardFieldError("cpu_cores_limit", 1.5)).toBe("settings.resources.error.cpuCores");
  });

  it("rss는 256 MiB..=1 TiB의 10진 문자열", () => {
    expect(guardFieldError("rss_limit_bytes", String(GUARD_RSS_MIN_BYTES))).toBeNull();
    expect(guardFieldError("rss_limit_bytes", String(GUARD_RSS_MAX_BYTES))).toBeNull();
    expect(guardFieldError("rss_limit_bytes", String(GUARD_RSS_MIN_BYTES - 1))).toBe("settings.resources.error.rssLimit");
    expect(guardFieldError("rss_limit_bytes", String(GUARD_RSS_MAX_BYTES + 1))).toBe("settings.resources.error.rssLimit");
    expect(guardFieldError("rss_limit_bytes", 4 * GiB)).toBe("settings.resources.error.rssLimit");
    expect(guardFieldError("rss_limit_bytes", "4G")).toBe("settings.resources.error.rssLimit");
  });

  it("두 지속 시간은 1000..=600000 ms", () => {
    for (const field of ["sustain_ms", "rss_sustain_ms"] as const) {
      expect(guardFieldError(field, "1000")).toBeNull();
      expect(guardFieldError(field, "600000")).toBeNull();
      expect(guardFieldError(field, "999")).toBe("settings.resources.error.sustain");
      expect(guardFieldError(field, "600001")).toBe("settings.resources.error.sustain");
    }
  });

  it("스위치는 boolean만", () => {
    expect(guardFieldError("auto_suspend", false)).toBeNull();
    expect(guardFieldError("auto_resume", "true")).toBe("settings.resources.error.type");
  });

  it("정책 전체에서 첫 문제 항목을 짚는다", () => {
    expect(guardPolicyProblem(FALLBACK_GUARD_POLICY)).toBeNull();
    expect(guardPolicyProblem({ ...FALLBACK_GUARD_POLICY, sustain_ms: "1" })).toEqual({
      field: "sustain_ms",
      key: "settings.resources.error.sustain",
    });
  });
});

describe("변경분 정리·합성", () => {
  it("저장된 변경분은 아는 항목·유효한 값만 남긴다", () => {
    expect(sanitizeGuardOverrides({ cpu_cores_limit: 2, rss_limit_bytes: "1", auto_resume: true, extra: 1 })).toEqual({
      cpu_cores_limit: 2,
      auto_resume: true,
    });
    expect(sanitizeGuardOverrides(null)).toEqual({});
    expect(sanitizeGuardOverrides("x")).toEqual({});
  });

  it("기본·현재 정책 위에 변경분을 얹는다", () => {
    const policy = overlayGuardPolicy(FALLBACK_GUARD_POLICY, { cpu_cores_limit: 3, auto_resume: true });
    expect(policy).toEqual({ ...FALLBACK_GUARD_POLICY, cpu_cores_limit: 3, auto_resume: true });
    expect(sameGuardPolicy(policy, { ...policy })).toBe(true);
    expect(sameGuardPolicy(policy, FALLBACK_GUARD_POLICY)).toBe(false);
  });

  it("바꾼 항목만 남기고, 기본값과 같아졌거나 되돌린 항목은 지운다", () => {
    const defaults: GuardPolicy = FALLBACK_GUARD_POLICY;
    let overrides = nextGuardOverrides({}, { cpu_cores_limit: 3 }, defaults);
    expect(overrides).toEqual({ cpu_cores_limit: 3 });
    overrides = nextGuardOverrides(overrides, { sustain_ms: "30000" }, defaults);
    expect(overrides).toEqual({ cpu_cores_limit: 3, sustain_ms: "30000" });
    overrides = nextGuardOverrides(overrides, { cpu_cores_limit: defaults.cpu_cores_limit }, defaults);
    expect(overrides).toEqual({ sustain_ms: "30000" });
    expect(nextGuardOverrides(overrides, {}, defaults, ["sustain_ms"])).toEqual({});
  });
});

describe("화면 단위 변환·호스트 기본값", () => {
  it("GiB·초 ↔ 바이트·밀리초", () => {
    expect(gibToBytes(2.5)).toBe(String(2.5 * GiB));
    expect(gibToBytes(Number.NaN)).toBeNull();
    expect(bytesToGib(String(3 * GiB))).toBe(3);
    expect(bytesToGib(String(GUARD_RSS_MIN_BYTES))).toBe(0.25);
    expect(secondsToMs(1.5)).toBe("1500");
    expect(msToSeconds("20000")).toBe(20);
    expect(msToSeconds("abc")).toBeNull();
  });

  it("데몬처럼 cpu = max(2, 코어/2), rss = RAM/4를 2–8 GiB로 자른다", () => {
    expect(hostDerivedGuardPolicy(12, 32 * GiB)).toMatchObject({ cpu_cores_limit: 6, rss_limit_bytes: String(8 * GiB) });
    expect(hostDerivedGuardPolicy(2, 4 * GiB)).toMatchObject({ cpu_cores_limit: 2, rss_limit_bytes: String(2 * GiB) });
    expect(hostDerivedGuardPolicy(16, 16 * GiB)).toMatchObject({ cpu_cores_limit: 8, rss_limit_bytes: String(4 * GiB) });
  });
});
