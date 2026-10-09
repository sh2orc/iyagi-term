import { describe, expect, it } from "vitest";
import { agentModelLabel, compactModelLabel, isAgentModelProvisional } from "./agentModel";

describe("compactModelLabel", () => {
  it("Claude 표시 이름의 1M 문구를 줄인다(상태줄·/model 결과 원문)", () => {
    expect(compactModelLabel("Opus 5 (1M context)")).toBe("Opus 5 (1M)");
    expect(compactModelLabel("Sonnet 5")).toBe("Sonnet 5");
  });

  it("Claude 모델 id(transcript assistant 항목)를 표시 이름 형태로", () => {
    expect(compactModelLabel("claude-opus-5")).toBe("Opus 5");
    expect(compactModelLabel("claude-opus-5[1m]")).toBe("Opus 5 (1M)");
    expect(compactModelLabel("claude-sonnet-4-6")).toBe("Sonnet 4.6");
    expect(compactModelLabel("claude-fable-5-1")).toBe("Fable 5.1");
    expect(compactModelLabel("claude-haiku-4-5-20251001")).toBe("Haiku 4.5");
  });

  it("실행 인수 별칭과 Codex 모델은 원문을 지킨다", () => {
    expect(compactModelLabel("opus[1m]")).toBe("opus (1M)");
    expect(compactModelLabel("sonnet")).toBe("sonnet");
    expect(compactModelLabel("gpt-5.6-sol")).toBe("gpt-5.6-sol");
    expect(compactModelLabel("  gpt-6-astra ")).toBe("gpt-6-astra");
  });
});

describe("agentModelLabel", () => {
  it("모델과 effort를 가운뎃점으로 잇고, 아는 것만 보인다", () => {
    expect(agentModelLabel({ model: "Opus 5 (1M context)", effort: "xhigh" })).toBe("Opus 5 (1M) · xhigh");
    expect(agentModelLabel({ model: "gpt-5.6-sol", effort: null })).toBe("gpt-5.6-sol");
    expect(agentModelLabel({ model: null, effort: "high" })).toBe("high");
    expect(agentModelLabel({})).toBeNull();
    expect(agentModelLabel({ model: "", effort: " " })).toBeNull();
  });

  it("잠정값은 defaults 출처뿐이다", () => {
    expect(isAgentModelProvisional({ model_source: "defaults" })).toBe(true);
    for (const source of ["status_line", "transcript", "rollout", "config", null, undefined] as const) {
      expect(isAgentModelProvisional({ model_source: source })).toBe(false);
    }
  });
});
