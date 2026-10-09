/**
 * 터미널 pane 배경색(pane마다 다르게).
 *
 * - 자동: 세션에서 감지된 에이전트(claude/codex/opencode, Claude가 GLM
 *   모델로 돌면 zai)마다 색조(hue)를 정의한다. 배경은 **어두운 테마 배경보다
 *   한 단계 어두운 면**에 그 색조를 입힌 것이고, 밝은 테마에서도 같은 면을
 *   쓴다 — 밝은 테마에서 밝기를 끌어오면 파스텔 면이 되어 눈에 거슬린다는
 *   지적이 있었다. 밝은 테마 pane은 이 어두운 면 위에서 글자가 읽히도록
 *   registry가 팔레트를 어두운 테마 것으로 바꾼다(registry.withBackground)
 *   (zai 구분은 배지 표시와 같은 판정: agentNames.agentDisplayId).
 * - 사용자 지정: pane마다 임의 배경색(오른쪽 클릭 → 배경색 지정). 지정하면
 *   테마와 무관하게 그 색을 **그대로** 쓴다(정확한 색이 필요할 때의 통로).
 * - 설정: 설정 → 터미널에서 켜기/끄기와 에이전트별 색조를 바꾼다.
 *
 * 이 모듈은 순수 계산만 한다(store/xterm 의존 없음 — node 시험 대상).
 * 실제 적용은 sessionController가 registry.setBackgroundOverride로 전달한다.
 */

import type { AgentStatus } from "../../generated/AgentStatus";
import { agentDisplayId } from "./agentNames";
import { DARK_THEME } from "./terminalPalette";

export type AgentTintKey = "claude" | "zai" | "codex" | "opencode";
export const AGENT_TINT_KEYS: readonly AgentTintKey[] = ["claude", "zai", "codex", "opencode"];

/**
 * 기본 색조 — 서로 확실히 구분되는 색상(hue)들. 여기서 쓰는 건 색상과
 * 채도뿐이고 밝기는 TINT_BASE_BACKGROUND에서 온다. 그래서 값이 밝아 보여도
 * 실제 배경은 어둡게 나온다(색상환 위치: 15°·256°·157°·216°로 네 방향에
 * 흩어 둔다).
 */
export const DEFAULT_AGENT_TINTS: Record<AgentTintKey, string> = {
  claude: "#e0521f", // Claude 테라코타
  zai: "#6d3df5", // Z.ai 보라
  codex: "#06a86b", // Codex 초록
  opencode: "#1f6fe8", // OpenCode 파랑
};

/**
 * 예전에 기본값으로 나갔던 색조들 — 저장된 설정이 이 값 그대로면 사용자가
 * 고른 색이 아니라 옛 기본값이므로 새 기본값으로 옮긴다(preferences.ts).
 * 이게 없으면 기본색을 바꿔도 한 번이라도 앱을 켠 사용자에게는 영영
 * 반영되지 않는다 — agentBackgroundColors가 localStorage에 저장되기 때문.
 */
export const LEGACY_AGENT_TINTS: Readonly<Record<AgentTintKey, readonly string[]>> = {
  claude: ["#c96442"],
  zai: ["#7a5af8"],
  codex: ["#189a6c"],
  opencode: ["#3d7bd9"],
};

/**
 * pane 배경의 채도 상한. 색조가 읽힐 만큼은 필요하고, 넘기면 배경이
 * 물감처럼 떠서 글자가 피로해진다. 사용자가 더 탁한 색을 고르면 그 색의
 * 낮은 채도를 그대로 쓴다(상한이지 고정값이 아니다).
 */
export const AGENT_TINT_SATURATION = 0.45;

/**
 * 기준 배경보다 얼마나 어둡게 낮출지(HSL 밝기 기준). **이 숫자가 "얼마나
 * 진한가"의 유일한 손잡이다** — 키우면 어두워지고 줄이면 밝아진다.
 *
 * 0.129 → 0.059. 어두운 테마 기본 배경(체감 밝기 31)보다 확실히 가라앉은
 * 면이 되어(claude #160c08 ≈ 15, codex #081611 ≈ 17) pane이 배경에 파묻히지
 * 않고 아래로 눌린 것처럼 읽힌다.
 */
export const AGENT_TINT_DARKEN = 0.07;
/**
 * 색조를 입힐 기준 배경. 밝은 테마여도 여기서 밝기를 가져온다 — 테마 배경
 * (#f7f5f1)에서 끌어오면 0.817짜리 파스텔 면이 되어 "너무 밝다".
 */
const TINT_BASE_BACKGROUND = DARK_THEME.background;
/** 새까만 테마에서도 색조가 보이도록 두는 최소 밝기. */
const MIN_TINT_LIGHTNESS = 0.04;

/** "#rrggbb" 형식인가(설정 저장·pane 지정 값 검증). */
export function isHexColor(value: unknown): value is string {
  return typeof value === "string" && /^#[0-9a-fA-F]{6}$/.test(value);
}

/** 감지된 에이전트 → 색조 키(모르는 에이전트/미감지는 null). */
export function agentTintKey(agent: AgentStatus | null | undefined): AgentTintKey | null {
  const id = agent ? agentDisplayId(agent.agent, agent.model) : null;
  return (AGENT_TINT_KEYS as readonly string[]).includes(id ?? "") ? (id as AgentTintKey) : null;
}

export interface Hsl {
  /** 색상 0..360. */
  h: number;
  /** 채도 0..1. */
  s: number;
  /** 밝기 0..1. */
  l: number;
}

/** "#rrggbb" → HSL. 형식이 어긋나면 null. */
export function hexToHsl(hex: string): Hsl | null {
  if (!isHexColor(hex)) return null;
  const r = parseInt(hex.slice(1, 3), 16) / 255;
  const g = parseInt(hex.slice(3, 5), 16) / 255;
  const b = parseInt(hex.slice(5, 7), 16) / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  const d = max - min;
  if (d === 0) return { h: 0, s: 0, l };
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  const h =
    max === r
      ? ((g - b) / d + (g < b ? 6 : 0)) * 60
      : max === g
        ? ((b - r) / d + 2) * 60
        : ((r - g) / d + 4) * 60;
  return { h, s, l };
}

/** HSL → "#rrggbb". */
export function hslToHex({ h, s, l }: Hsl): string {
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const hp = (((h % 360) + 360) % 360) / 60;
  const x = c * (1 - Math.abs((hp % 2) - 1));
  const [r1, g1, b1] =
    hp < 1
      ? [c, x, 0]
      : hp < 2
        ? [x, c, 0]
        : hp < 3
          ? [0, c, x]
          : hp < 4
            ? [0, x, c]
            : hp < 5
              ? [x, 0, c]
              : [c, 0, x];
  const m = l - c / 2;
  const part = (v: number) =>
    Math.round(Math.min(1, Math.max(0, v + m)) * 255)
      .toString(16)
      .padStart(2, "0");
  return `#${part(r1)}${part(g1)}${part(b1)}`;
}

/**
 * 색조를 입힌 pane 배경 — 테마와 무관하게 어두운 면이다.
 *
 * 색조에서 가져오는 건 색상과 채도뿐이다. 밝기를 색조에서 가져오면 밝은
 * 색을 고를수록 배경이 밝아져서(예전 방식) 어두운 테마가 무너진다. 밝기를
 * 테마 배경에서 가져와도(그다음 방식) 밝은 테마에서 파스텔 면이 나와
 * 거슬린다 — 그래서 밝은 테마면 어두운 테마 배경을 기준으로 바꿔 쓴다.
 * 어두운 테마 계열이면 그 테마 배경을 그대로 기준으로 삼는다(테마 배경이
 * 더 어두우면 pane도 그만큼 더 어두워진다).
 *
 * 입력 형식이 어긋나면 테마 배경을 그대로 돌려준다.
 */
export function tintedSurface(themeBackground: string, tint: string): string {
  const theme = hexToHsl(themeBackground);
  const hue = hexToHsl(tint);
  if (!theme || !hue) return themeBackground;
  const base = theme.l > 0.5 ? hexToHsl(TINT_BASE_BACKGROUND)! : theme;
  return hslToHex({
    h: hue.h,
    s: Math.min(hue.s, AGENT_TINT_SATURATION),
    l: Math.max(MIN_TINT_LIGHTNESS, base.l - AGENT_TINT_DARKEN),
  });
}

export interface PaneBackgroundInput {
  /** pane에 사용자가 직접 고른 배경색(가장 우선). */
  custom: string | null | undefined;
  /** 세션에서 감지된 에이전트. */
  agent: AgentStatus | null | undefined;
}

export interface PaneBackgroundPrefs {
  agentBackgrounds: boolean;
  agentBackgroundColors: Record<AgentTintKey, string>;
}

/**
 * 이 pane에 적용할 배경색(없으면 null = 테마 기본).
 * 우선순위: 사용자 지정 > 에이전트 색조(설정이 켜져 있을 때) > 테마.
 */
export function paneBackgroundColor(
  prefs: PaneBackgroundPrefs,
  pane: PaneBackgroundInput,
  themeBackground: string,
): string | null {
  if (pane.custom) return pane.custom;
  if (!prefs.agentBackgrounds) return null;
  const key = agentTintKey(pane.agent);
  if (!key) return null;
  const tint = prefs.agentBackgroundColors[key] ?? DEFAULT_AGENT_TINTS[key];
  return tintedSurface(themeBackground, tint);
}
