/**
 * 타이틀 앞 활동 마커의 판정: 에이전트 자체 보고(Claude 레지스트리 status)가
 * 있으면 그것을, 없으면 출력 활동을 근거로 한다.
 */

import { describe, expect, it } from "vitest";
import { t } from "../../i18n";
import { agentActivityFromStatus, agentActivityLabel, agentDisplayName, agentTitleLabel, isAgentBusy, isKnownAgent, resolveAgentActivity } from "./agentNames";

describe("agentActivityFromStatus", () => {
  it("Claude 레지스트리 status 넷을 세 상태로 접는다", () => {
    expect(agentActivityFromStatus("busy")).toBe("working");
    expect(agentActivityFromStatus("shell")).toBe("working");
    expect(agentActivityFromStatus("waiting")).toBe("waiting");
    expect(agentActivityFromStatus("idle")).toBe("idle");
  });

  it("모르는 값·없음은 null — 추측하지 않는다", () => {
    expect(agentActivityFromStatus(null)).toBeNull();
    expect(agentActivityFromStatus(undefined)).toBeNull();
    expect(agentActivityFromStatus("")).toBeNull();
    expect(agentActivityFromStatus("BUSY")).toBeNull();
  });
});

describe("resolveAgentActivity", () => {
  it("자체 보고가 있으면 출력 활동과 무관하게 그것을 믿는다", () => {
    expect(resolveAgentActivity({ session_status: "idle" }, true)).toBe("idle");
    expect(resolveAgentActivity({ session_status: "busy" }, false)).toBe("working");
    expect(resolveAgentActivity({ session_status: "waiting" }, true)).toBe("waiting");
  });

  it("자체 보고가 없으면(Codex·opencode) 출력 활동으로 대신한다", () => {
    expect(resolveAgentActivity({ session_status: null }, true)).toBe("working");
    expect(resolveAgentActivity({ session_status: null }, false)).toBe("idle");
    expect(resolveAgentActivity({ session_status: undefined }, true)).toBe("working");
  });
});

describe("agentActivityLabel / isAgentBusy", () => {
  it("idle은 마커가 없으므로 문구도 없다", () => {
    expect(agentActivityLabel("idle", "claude")).toBeNull();
  });

  it("작업 중·확인 대기 문구에 에이전트 표시 이름을 넣는다", () => {
    expect(agentActivityLabel("working", "claude")).toBe(
      t("terminal.agent.working", { name: t("terminal.agent.claude") }),
    );
    expect(agentActivityLabel("waiting", "codex")).toBe(
      t("terminal.agent.waiting", { name: t("terminal.agent.codex") }),
    );
    // 모르는 에이전트 id는 그대로 이름으로 쓴다(깨지지 않게).
    expect(agentActivityLabel("working", "gemini")).toBe(t("terminal.agent.working", { name: "gemini" }));
  });

  it("isAgentBusy는 busy·shell만 참이다", () => {
    expect(isAgentBusy({ session_status: "busy" })).toBe(true);
    expect(isAgentBusy({ session_status: "shell" })).toBe(true);
    expect(isAgentBusy({ session_status: "waiting" })).toBe(false);
    expect(isAgentBusy({ session_status: null })).toBe(false);
  });
});

describe("Kimi CLI provider 표시", () => {
  it("Kimi provider와 다른 provider를 구분하고 CLI 자체를 인식한다", () => {
    expect(isKnownAgent("kimi")).toBe(true);
    expect(agentDisplayName("kimi", "K3", "Kimi Code")).toBe("Kimi Code");
    expect(agentDisplayName("kimi", "gpt-4.1", "openai")).toBe("Kimi CLI · openai");
    expect(agentDisplayName("kimi", "kimi-k3.1-pro", "Kimi Open Platform")).toBe("Kimi CLI · Kimi Open Platform");
    expect(agentDisplayName("kimi", null, null)).toBe("Kimi CLI");
  });
});

describe("agentTitleLabel — 창 제목의 provider·모델 표시", () => {
  it("Kimi는 provider 이름과 모델·effort를 함께 붙인다", () => {
    expect(agentTitleLabel({ agent: "kimi", provider: "Kimi Code", model: "K3.1 Pro", effort: "high" })).toBe(
      "Kimi Code · K3.1 Pro · high",
    );
    expect(agentTitleLabel({ agent: "kimi", provider: null, model: null, effort: null })).toBe("Kimi CLI");
  });

  it("다른 에이전트도 이름·모델이 같이 읽힌다(GLM Claude는 Z.ai)", () => {
    expect(agentTitleLabel({ agent: "claude", model: "glm-5.3", effort: null, provider: null })).toBe(
      "Z.ai · glm-5.3",
    );
    expect(agentTitleLabel({ agent: "codex", model: "gpt-5.6-sol", effort: "high", provider: null })).toBe(
      "Codex · gpt-5.6-sol · high",
    );
  });
});
