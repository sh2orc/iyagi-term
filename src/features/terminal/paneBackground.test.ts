/**
 * pane 배경색 순수 계산(paneBackground.ts):
 * - tint 키 판정 — Claude가 GLM 모델이면 zai로 구분한다(배지 표시와 같은 판정).
 * - 면 만들기 — 어두운 면에 색조(hue)만 입힌다(밝은 테마도 같은 면).
 * - 우선순위 — 사용자 지정 > 에이전지 tint(설정 on) > 테마 기본(null).
 */

import { describe, expect, it } from "vitest";
import type { AgentStatus } from "../../generated/AgentStatus";
import {
  agentTintKey,
  DEFAULT_AGENT_TINTS,
  hexToHsl,
  hslToHex,
  isHexColor,
  paneBackgroundColor,
  tintedSurface,
  type PaneBackgroundPrefs,
} from "./paneBackground";

const DARK = "#1e1f24";
const LIGHT = "#f7f5f1";

/** 눈이 느끼는 밝기(ITU-R BT.601) — "더 어두운가"를 따지는 잣대. */
function brightness(hex: string): number {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
  return 0.299 * r + 0.587 * g + 0.114 * b;
}

function agent(id: string, model: string | null = null): AgentStatus {
  return {
    agent: id,
    pid: 1,
    detected_at_ms: 0,
    session_id: null,
    session_name: null,
    model,
    effort: null,
  };
}

function prefs(overrides: Partial<PaneBackgroundPrefs> = {}): PaneBackgroundPrefs {
  return {
    agentBackgrounds: true,
    agentBackgroundColors: { ...DEFAULT_AGENT_TINTS },
    ...overrides,
  };
}

describe("hexToHsl / hslToHex", () => {
  it("왕복해도 같은 색이다", () => {
    for (const hex of ["#1e1f24", "#f7f5f1", "#e0521f", "#06a86b", "#000000", "#ffffff"]) {
      expect(hslToHex(hexToHsl(hex)!)).toBe(hex);
    }
  });
  it("무채색은 채도 0", () => {
    expect(hexToHsl("#808080")!.s).toBe(0);
  });
  it("형식이 어긋나면 null", () => {
    expect(hexToHsl("red")).toBeNull();
    expect(hexToHsl("#12345")).toBeNull();
  });
});

describe("tintedSurface", () => {
  it("어두운 테마에서 배경보다 더 어두워진다(색을 섞어 밝아지지 않는다)", () => {
    for (const tint of Object.values(DEFAULT_AGENT_TINTS)) {
      expect(brightness(tintedSurface(DARK, tint))).toBeLessThan(brightness(DARK));
    }
  });
  it("밝은 테마에서도 어두운 테마와 똑같은 면을 쓴다", () => {
    for (const tint of Object.values(DEFAULT_AGENT_TINTS)) {
      expect(tintedSurface(LIGHT, tint)).toBe(tintedSurface(DARK, tint));
      // 밝은 테마 배경(체감 밝기 ≈ 245)의 절반도 안 되게 가라앉는다.
      expect(brightness(tintedSurface(LIGHT, tint))).toBeLessThan(brightness(LIGHT) / 2);
    }
  });
  it("어두운 테마가 더 어두우면 면도 그만큼 더 어두워진다", () => {
    expect(brightness(tintedSurface("#000000", "#e0521f"))).toBeLessThan(
      brightness(tintedSurface(DARK, "#e0521f")),
    );
  });
  it("색조의 밝기는 쓰지 않는다 — 같은 색상이면 밝든 어둡든 같은 면이 나온다", () => {
    // 같은 색상(0°)·같은 채도, 밝기만 다른 두 색.
    expect(tintedSurface(DARK, "#ff8080")).toBe(tintedSurface(DARK, "#800000"));
  });
  it("색조마다 다른 색이 나온다", () => {
    const surfaces = Object.values(DEFAULT_AGENT_TINTS).map((t) => tintedSurface(DARK, t));
    expect(new Set(surfaces).size).toBe(surfaces.length);
  });
  it("형식이 어긋난 입력은 테마 배경을 그대로 돌려준다", () => {
    expect(tintedSurface(DARK, "red")).toBe(DARK);
    expect(tintedSurface("nope", "#e0521f")).toBe("nope");
  });
});

describe("agentTintKey", () => {
  it("감지된 에이전트마다 키를 내고 GLM 모델의 Claude는 zai로 구분한다", () => {
    expect(agentTintKey(agent("claude"))).toBe("claude");
    expect(agentTintKey(agent("claude", "glm-5.3[1m]"))).toBe("zai");
    expect(agentTintKey(agent("codex", "gpt-5.6-sol"))).toBe("codex");
    expect(agentTintKey(agent("opencode"))).toBe("opencode");
  });
  it("모르는 에이전트와 미감지는 null", () => {
    expect(agentTintKey(agent("cursor"))).toBeNull();
    expect(agentTintKey(null)).toBeNull();
    expect(agentTintKey(undefined)).toBeNull();
  });
});

describe("paneBackgroundColor", () => {
  it("사용자 지정 색이 가장 우선이다(에이전트·설정과 무관하게 그대로)", () => {
    expect(
      paneBackgroundColor(prefs(), { custom: "#123456", agent: agent("codex") }, DARK),
    ).toBe("#123456");
  });
  it("설정이 꺼져 있으면 tint 없이 테마 기본(null)", () => {
    expect(
      paneBackgroundColor(prefs({ agentBackgrounds: false }), { custom: null, agent: agent("claude") }, DARK),
    ).toBeNull();
  });
  it("에이전트가 없으면 테마 기본(null)", () => {
    expect(paneBackgroundColor(prefs(), { custom: null, agent: null }, DARK)).toBeNull();
  });
  it("감지된 에이전트는 사용자 색조를 입힌 어두운 면이다", () => {
    const colors = { ...DEFAULT_AGENT_TINTS, codex: "#0000ff" };
    const color = paneBackgroundColor(prefs({ agentBackgroundColors: colors }), { custom: null, agent: agent("codex") }, DARK);
    expect(color).toBe(tintedSurface(DARK, "#0000ff"));
    expect(color).not.toBe(DARK);
    expect(brightness(color!)).toBeLessThan(brightness(DARK));
  });
  it("zai와 claude는 다른 색이 나온다", () => {
    const zai = paneBackgroundColor(prefs(), { custom: null, agent: agent("claude", "glm-5.3[1m]") }, DARK);
    const claude = paneBackgroundColor(prefs(), { custom: null, agent: agent("claude") }, DARK);
    expect(zai).not.toBe(claude);
  });
});

describe("isHexColor", () => {
  it("#rrggbb만 받는다", () => {
    expect(isHexColor("#1e1f24")).toBe(true);
    expect(isHexColor("#1E1F24")).toBe(true);
    expect(isHexColor("1e1f24")).toBe(false);
    expect(isHexColor("#1e1f2")).toBe(false);
    expect(isHexColor(null)).toBe(false);
  });
});
