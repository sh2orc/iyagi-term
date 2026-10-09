/**
 * 역할 가능 판정과 증거 층(11 §3.1·§8): 실험 허용으로 `supported: true`가 된 기능도 역할
 * 판정을 통과하고, 신뢰 칩·`실험적 연결` 배지는 **필수 기능**(structured_result·events·cancel과
 * 접근 모드 하나)만 본다. 로컬 증거 요약은 칩 아래 한 줄의 조각을 그대로 돌려준다.
 */

import { describe, expect, it } from "vitest";
import type { Binding } from "../../generated/Binding";
import type { LocalEvidence } from "../../generated/LocalEvidence";
import {
  bindingSupportsRole,
  bindingTrust,
  isExperimentalBinding,
  localEvidenceSummary,
  roleTaskKind,
} from "./bindingSupport";
import { newBinding } from "./configuration";

function withCapabilities(reason: string | null, supported = true): Binding {
  const binding = newBinding("claude");
  for (const key of Object.keys(binding.capabilities) as Array<keyof Binding["capabilities"]>) {
    binding.capabilities[key] = { supported, reason_code: reason };
  }
  return binding;
}

const experimental = { supported: true, reason_code: "experimental_opt_in" };
const localProbe = { supported: true, reason_code: "local_probe" };
const observed = { supported: true, reason_code: "observed_runs" };
const versionLine = { supported: true, reason_code: "version_line" };
const shipped = { supported: true, reason_code: null };

function evidence(overrides: Partial<LocalEvidence> = {}): LocalEvidence {
  return {
    os: "macos", version: "0.154.0", model_id: "gpt-5.6-luna", probed_at: "2026-09-19T00:00:00Z",
    probe: { protocol_ok: true, sandbox_cases_passed: 12, sandbox_cases_total: 12, model_listed: true, failures: [] },
    runs: { succeeded_read_only: 2, succeeded_write: 1, cancelled: 1, invalid_result: 0, last_at: "2026-09-19T01:00:00Z" },
    ...overrides,
  };
}

describe("bindingSupportsRole", () => {
  it("treats experimental opt-in capabilities as supported without reading the reason", () => {
    const binding = withCapabilities("experimental_opt_in");
    for (const role of ["lead", "builder", "reviewer", "integrator"] as const) {
      expect(bindingSupportsRole(binding, role)).toBe(true);
    }
  });

  it("keeps roles blocked when the adapter does not implement a needed capability", () => {
    const binding = withCapabilities("experimental_opt_in");
    binding.capabilities.scoped_write = { supported: false, reason_code: "adapter_not_implemented" };
    expect(bindingSupportsRole(binding, "lead")).toBe(true);
    expect(bindingSupportsRole(binding, "builder")).toBe(false);
    expect(bindingSupportsRole(binding, "integrator")).toBe(false);
    expect(bindingSupportsRole({ ...withCapabilities(null), enabled: false }, "lead")).toBe(false);
  });

  it("maps every role to the task kind its required capabilities are judged by", () => {
    expect(roleTaskKind("lead")).toBe("plan");
    expect(roleTaskKind("builder")).toBe("implement");
    expect(roleTaskKind("integrator")).toBe("integrate");
  });
});

describe("isExperimentalBinding", () => {
  it("is true only when a required capability was opened by consent", () => {
    const binding = withCapabilities(null);
    expect(isExperimentalBinding(binding)).toBe(false);
    binding.capabilities.cancel = experimental;
    expect(isExperimentalBinding(binding)).toBe(true);
    expect(isExperimentalBinding(withCapabilities("no_compatibility_evidence", false))).toBe(false);
    expect(isExperimentalBinding(null)).toBe(false);
    expect(isExperimentalBinding(undefined)).toBe(false);
  });

  it("ignores optional capabilities — steer/resume consent is not an experimental connection", () => {
    const binding = withCapabilities(null);
    binding.capabilities.steer = experimental;
    binding.capabilities.resume = experimental;
    binding.capabilities.approval_reply = experimental;
    binding.capabilities.model_listing = experimental;
    expect(isExperimentalBinding(binding)).toBe(false);
    expect(isExperimentalBinding(binding, "implement")).toBe(false);
  });

  it("judges the access mode the task kind needs, and both when no kind is at hand", () => {
    const binding = withCapabilities(null);
    binding.capabilities.scoped_write = experimental;
    expect(isExperimentalBinding(binding, "plan")).toBe(false);
    expect(isExperimentalBinding(binding, "review")).toBe(false);
    expect(isExperimentalBinding(binding, "implement")).toBe(true);
    expect(isExperimentalBinding(binding, "document")).toBe(true);
    expect(isExperimentalBinding(binding)).toBe(true);
  });
});

describe("bindingTrust", () => {
  it("is shipped when every required capability comes from release evidence (exact or version line)", () => {
    expect(bindingTrust(withCapabilities(null))).toBe("shipped");
    expect(bindingTrust(withCapabilities("version_line"))).toBe("shipped");
    const mixed = withCapabilities(null);
    mixed.capabilities.events = versionLine;
    // 선택 기능이 어떤 사유든 칩은 바뀌지 않는다.
    mixed.capabilities.steer = { supported: false, reason_code: "adapter_not_implemented" };
    expect(bindingTrust(mixed)).toBe("shipped");
  });

  it("is local when this machine's probe or observed runs opened a required capability", () => {
    const probed = withCapabilities(null);
    probed.capabilities.cancel = localProbe;
    expect(bindingTrust(probed)).toBe("local");
    const runs = withCapabilities("local_probe");
    runs.capabilities.scoped_write = observed;
    expect(bindingTrust(runs)).toBe("local");
    // 쓰기 기능만 로컬 증거라면 읽기 전용 할 일은 여전히 출시 증거로 열린다.
    const write = withCapabilities(null);
    write.capabilities.scoped_write = localProbe;
    expect(bindingTrust(write, "plan")).toBe("shipped");
    expect(bindingTrust(write, "implement")).toBe("local");
  });

  it("is experimental as soon as one required capability rests on consent", () => {
    const binding = withCapabilities("local_probe");
    binding.capabilities.read_only = experimental;
    expect(bindingTrust(binding)).toBe("experimental");
    expect(bindingTrust(binding, "implement")).toBe("local");
    expect(bindingTrust(withCapabilities("experimental_opt_in"))).toBe("experimental");
  });

  it("is unverified without a required capability or with an unclaimed reason", () => {
    expect(bindingTrust(null)).toBe("unverified");
    expect(bindingTrust(withCapabilities("no_compatibility_evidence", false))).toBe("unverified");
    const blocked = withCapabilities(null);
    blocked.capabilities.cancel = { supported: false, reason_code: "local_probe_failed" };
    expect(bindingTrust(blocked)).toBe("unverified");
    // 출시 fixture가 남긴 미증명 slug는 증거 층이 아니다.
    const unknown = withCapabilities(null);
    unknown.capabilities.events = { supported: true, reason_code: "print_mode_no_approval_reply" };
    expect(bindingTrust(unknown)).toBe("unverified");
    // 접근 모드 중 열린 쪽만 본다: 쓰기가 막혀 있어도 읽기 전용 증거는 그대로 인정된다.
    const readOnly = withCapabilities(null);
    readOnly.capabilities.scoped_write = { supported: false, reason_code: "local_probe_failed" };
    expect(bindingTrust(readOnly)).toBe("shipped");
    expect(bindingTrust(readOnly, "implement")).toBe("unverified");
  });

  it("keeps the shipped chip on a connection whose access modes are both release-verified", () => {
    const binding = withCapabilities(null);
    binding.capabilities.read_only = shipped;
    binding.capabilities.scoped_write = shipped;
    expect(bindingTrust(binding)).toBe("shipped");
  });
});

describe("localEvidenceSummary", () => {
  it("returns the pieces of the one-line rationale, with successful runs summed", () => {
    const binding = { ...withCapabilities(null), local_evidence: evidence() };
    expect(localEvidenceSummary(binding)).toEqual({
      probed: true, protocolOk: true, sandboxPassed: 12, sandboxTotal: 12, runs: 3,
      probedAt: "2026-09-19T00:00:00Z", modelListed: true,
    });
  });

  it("keeps missing pieces as null instead of inventing them", () => {
    const binding = {
      local_evidence: evidence({
        probe: { protocol_ok: true, sandbox_cases_passed: null, sandbox_cases_total: null, model_listed: false, failures: [] },
        runs: { succeeded_read_only: 0, succeeded_write: 0, cancelled: 0, invalid_result: 0, last_at: null },
        probed_at: null,
      }),
    };
    expect(localEvidenceSummary(binding)).toEqual({
      probed: true, protocolOk: true, sandboxPassed: null, sandboxTotal: null, runs: 0, probedAt: null, modelListed: false,
    });
  });

  it("is null without evidence, and null when nothing has been measured yet", () => {
    expect(localEvidenceSummary(null)).toBeNull();
    expect(localEvidenceSummary(undefined)).toBeNull();
    expect(localEvidenceSummary(newBinding())).toBeNull();
    expect(localEvidenceSummary({ local_evidence: null })).toBeNull();
    const empty = evidence({
      probe: null,
      runs: { succeeded_read_only: 0, succeeded_write: 0, cancelled: 0, invalid_result: 0, last_at: null },
    });
    expect(localEvidenceSummary({ local_evidence: empty })).toBeNull();
    // 자가 진단은 없어도 성공 실행 관측이 있으면 보여 줄 근거가 된다.
    const runsOnly = evidence({ probe: null, runs: { succeeded_read_only: 3, succeeded_write: 0, cancelled: 0, invalid_result: 0, last_at: null } });
    expect(localEvidenceSummary({ local_evidence: runsOnly })).toMatchObject({ probed: false, protocolOk: false, runs: 3, sandboxTotal: null });
  });
});
