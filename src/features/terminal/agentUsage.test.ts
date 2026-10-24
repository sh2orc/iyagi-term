/**
 * 사용량 한도 배지 판정(agentUsage) — 공식 관찰값(퍼센트·리셋 시각)만 따르는지.
 */

import { describe, expect, it } from "vitest";
import type { AgentStatus } from "../../generated/AgentStatus";
import { blockedWindow, tabUsageLimitResetsAt, usageLimitVisible } from "./agentUsage";
import { makeRoot, type PaneLike } from "./agentUsageTestUtils";

function agent(limits: AgentStatus["usage_limits"]): AgentStatus {
  return {
    agent: "claude",
    pid: 7,
    detected_at_ms: 1,
    usage_limits: limits,
  } as AgentStatus;
}

function limits(
  fiveHour: [number, number | null] | null,
  sevenDay?: [number, number | null] | null,
): AgentStatus["usage_limits"] {
  const window = (pair: [number, number | null] | null | undefined) =>
    pair
      ? { used_percent: pair[0], resets_at_unix_ms: pair[1] as number | null }
      : null;
  return {
    five_hour: window(fiveHour) as never,
    seven_day: window(sevenDay) as never,
    observed_at_unix_ms: 1_000,
  };
}

describe("blockedWindow", () => {
  it("100% 이상인 창만 본다", () => {
    expect(blockedWindow(agent(limits([99, 500])))).toBeNull();
    expect(blockedWindow(agent(limits([100, 500])))).not.toBeNull();
    expect(blockedWindow(agent(null))).toBeNull();
    expect(blockedWindow(agent(undefined))).toBeNull();
  });

  it("여러 창이 막혔으면 리셋이 가장 이른 것이 이긴다", () => {
    const best = blockedWindow(agent(limits([100, 900], [100, 300])));
    expect(best?.resets_at_unix_ms).toBe(300);
  });

  it("리셋을 모르는 창은 정적 폴백 — 시각을 아는 창이 있으면 진다", () => {
    expect(blockedWindow(agent(limits([100, null])))?.resets_at_unix_ms).toBeNull();
    const timed = blockedWindow(agent(limits([100, null], [100, 700])));
    expect(timed?.resets_at_unix_ms).toBe(700);
  });
});

describe("usageLimitVisible", () => {
  it("리셋이 지나도 유예 동안은 보인다", () => {
    expect(usageLimitVisible(1_000, 1_500)).toBe(true);
    expect(usageLimitVisible(1_000, 1_000 + 120_000)).toBe(true);
    expect(usageLimitVisible(1_000, 1_001 + 120_000)).toBe(false);
    expect(usageLimitVisible(null, 0)).toBe(true);
  });
});

describe("tabUsageLimitResetsAt", () => {
  it("탭 전체에서 가장 이른 리셋을 고르고 없음/정적을 구분한다", () => {
    const panes: Record<string, PaneLike | undefined> = {
      a: { agent: agent(limits([100, 900])) },
      b: { agent: agent(limits([42, 100])) },
    };
    expect(tabUsageLimitResetsAt(panes, makeRoot(["a", "b"]))).toBe(900);

    panes.b = { agent: agent(limits([100, null])) };
    expect(tabUsageLimitResetsAt(panes, makeRoot(["a", "b"]))).toBe(900);

    panes.a = { agent: agent(limits([30, 100])) };
    expect(tabUsageLimitResetsAt(panes, makeRoot(["a", "b"]))).toBeNull();

    panes.b = { agent: null };
    expect(tabUsageLimitResetsAt(panes, makeRoot(["a", "b"]))).toBeUndefined();
  });
});
