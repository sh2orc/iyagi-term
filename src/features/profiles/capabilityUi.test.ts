/**
 * capability 현실/게이트 (03 §4·§8, 04 §7): require + 미지원 → 실행 차단
 * (CAPABILITY_UNAVAILABLE 설명), permission_required 사유 노출,
 * prefer/observe는 차단하지 않고 누락을 알린다.
 */

import { describe, expect, it } from "vitest";
import type { Capabilities } from "../../generated/Capabilities";
import { translate } from "../../i18n";
import { ENFORCEMENT_OPTIONS, LIMIT_LABELS, capabilityGate, limitRealities, limitSupportText } from "./capabilityUi";

function caps(overrides: Partial<Capabilities> = {}): Capabilities {
  return {
    memory_limit_kind: { support: "supported" },
    cpu_quota: { support: "supported" },
    process_count_limit: { support: "supported" },
    tree_accounting: { support: "supported" },
    reattach: { support: "supported" },
    resume: { support: "unsupported", reason: "R1: daemon 재시작 후 세션 resume 없음" },
    scheduling_yield: { support: "supported" },
    suspend_resume: { support: "supported" },
    platform: "windows",
    claude_provider_routing: false,
    ...overrides,
  };
}

const MAC_REASONS = caps({
  memory_limit_kind: { support: "unsupported", reason: "macOS R1은 memory hard cap 미지원" },
  cpu_quota: { support: "unsupported", reason: "macOS R1은 CPU quota 미지원" },
  process_count_limit: { support: "unsupported", reason: "관측 트리 방식" },
  tree_accounting: { support: "unsupported", reason: "별도 session 자손은 관측에서 누락 가능" },
  platform: "darwin",
});

const ALL_REQUESTED = {
  enforcement: "require" as const,
  memoryMaxBytes: 4 * 1024 ** 3,
  cpuMaxCores: 2,
  pidsMax: 128,
};

describe("limitRealities — 요청한 상한의 실제 적용 현실", () => {
  it("모두 지원되면 차단/경고 없음", () => {
    const realities = limitRealities(ALL_REQUESTED, caps());
    expect(realities.every((r) => r.support === "supported" && !r.blocking && r.notice === null)).toBe(true);
  });

  it("require + 미지원 상한은 blocking이고 이유가 노출된다", () => {
    const realities = limitRealities(ALL_REQUESTED, MAC_REASONS);
    const memory = realities.find((r) => r.key === "memory_max_bytes");
    expect(memory?.blocking).toBe(true);
    expect(memory?.supportText).toBe("미지원");
    expect(memory?.notice).toContain("macOS R1은 memory hard cap 미지원");
  });

  it("permission_required도 require에서는 차단이며 사유가 보인다", () => {
    const permissionCaps = caps({
      memory_limit_kind: { support: "permission_required", reason: "cgroup v2 위임 subtree 없음" },
    });
    const realities = limitRealities(ALL_REQUESTED, permissionCaps);
    const memory = realities.find((r) => r.key === "memory_max_bytes");
    expect(memory?.supportText).toBe("권한 필요");
    expect(memory?.blocking).toBe(true);
    expect(memory?.notice).toContain("cgroup v2 위임 subtree 없음");
  });

  it("요청하지 않은 상한의 미지원은 알림을 만들지 않는다", () => {
    const realities = limitRealities(
      { enforcement: "require", memoryMaxBytes: null, cpuMaxCores: null, pidsMax: null },
      MAC_REASONS,
    );
    expect(realities.every((r) => !r.blocking && r.notice === null)).toBe(true);
  });

  it("prefer는 차단하지 않고 누락을 알린다", () => {
    const realities = limitRealities({ ...ALL_REQUESTED, enforcement: "prefer" }, MAC_REASONS);
    expect(realities.every((r) => !r.blocking)).toBe(true);
    expect(realities.find((r) => r.key === "memory_max_bytes")?.notice).toContain("누락");
  });

  it("observe는 상한이 있어도 '적용하지 않는다'고만 알린다", () => {
    const realities = limitRealities({ ...ALL_REQUESTED, enforcement: "observe" }, caps());
    const memory = realities.find((r) => r.key === "memory_max_bytes");
    expect(memory?.blocking).toBe(false);
    expect(memory?.notice).toContain("관측만");
  });

  it("지원 상태 문구", () => {
    expect(limitSupportText("supported")).toBe("적용 가능");
    expect(limitSupportText("unsupported")).toBe("미지원");
    expect(limitSupportText("permission_required")).toBe("권한 필요");
  });
});

describe("capabilityGate — require + 미지원 → 실행 차단", () => {
  it("blocked면 CAPABILITY_UNAVAILABLE 설명과 미지원 목록·사유를 낸다", () => {
    const gate = capabilityGate(ALL_REQUESTED, MAC_REASONS);
    expect(gate.blocked).toBe(true);
    expect(gate.blockedLimits.map((l) => l.key)).toContain("memory_max_bytes");
    expect(gate.message).toContain("CAPABILITY_UNAVAILABLE");
    expect(gate.message).toContain("메모리 상한");
    expect(gate.message).toContain("macOS R1은 memory hard cap 미지원");
    expect(gate.message).toContain("가능하면 적용");
  });

  it("permission_required 사유도 차단 메시지에 포함된다", () => {
    const gate = capabilityGate(
      ALL_REQUESTED,
      caps({ cpu_quota: { support: "permission_required", reason: "processor group 다중 호스트" } }),
    );
    expect(gate.blocked).toBe(true);
    expect(gate.message).toContain("권한 필요");
    expect(gate.message).toContain("processor group 다중 호스트");
  });

  it("요청 상한이 모두 적용 가능하면 차단하지 않는다", () => {
    expect(capabilityGate(ALL_REQUESTED, caps()).blocked).toBe(false);
    expect(
      capabilityGate({ enforcement: "require", memoryMaxBytes: null, cpuMaxCores: null, pidsMax: null }, MAC_REASONS)
        .blocked,
    ).toBe(false);
  });

  it("prefer/observe는 미지원이어도 차단하지 않는다", () => {
    expect(capabilityGate({ ...ALL_REQUESTED, enforcement: "prefer" }, MAC_REASONS).blocked).toBe(false);
    expect(capabilityGate({ ...ALL_REQUESTED, enforcement: "observe" }, MAC_REASONS).blocked).toBe(false);
  });
});

describe("기본 프로필 정책(03 §8)과 세그먼트 라벨", () => {
  it("기본값(observe, cap 없음)은 어떤 capabilities에서도 차단하지 않는다", () => {
    const defaults = { enforcement: "observe" as const, memoryMaxBytes: null, cpuMaxCores: null, pidsMax: null };
    expect(capabilityGate(defaults, MAC_REASONS).blocked).toBe(false);
    expect(capabilityGate(defaults, caps()).blocked).toBe(false);
  });

  it("세그먼트 라벨: 관측만 / 가능하면 적용 / 필수", () => {
    expect(ENFORCEMENT_OPTIONS.map((o) => o.label)).toEqual(["관측만", "가능하면 적용", "필수"]);
    expect(ENFORCEMENT_OPTIONS.map((o) => o.value)).toEqual(["observe", "prefer", "require"]);
  });
});

describe("en 번역 검증 (i18n 키 → 영어 문구)", () => {
  it("상한 라벨/지원 문구가 영어 사전으로 나온다", () => {
    expect(translate("en", "caps.limit.memory_max_bytes")).toBe("Memory limit");
    expect(LIMIT_LABELS.memory_max_bytes).toBe("메모리 상한"); // 접근 시점 평가(getter)
    expect(translate("en", "caps.support.supported")).toBe("Enforceable");
    expect(translate("en", "caps.gate.line1")).toContain("CAPABILITY_UNAVAILABLE");
  });
});
