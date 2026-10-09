/**
 * 로컬 사용자 설정의 단일 원본(04-ui.md §1 설정 페이지가 읽고 쓴다).
 *
 * - 값은 전부 localStorage에 지속하며 스키마가 어긋난 값은 조용히 기본값으로
 *   되돌린다(i18n 스토어 merge 가드와 같은 패턴). 저장 키는 예전 이름을
 *   그대로 쓴다 — 이미 배포된 `terminateOnClose` 값을 마이그레이션 코드
 *   없이 이어받기 위해서다.
 * - 터미널 출력·환경 변수는 절대 넣지 않는다(04 §4).
 * - 설정 항목을 추가하는 자리는 세 곳뿐이다: PreferenceValues,
 *   DEFAULT_PREFERENCES, sanitize. 그러면 setter·되돌리기·검색이 따라온다.
 */

import { create } from "zustand";
import { createJSONStorage, persist } from "zustand/middleware";
import { DEFAULT_FONT_SIZE } from "../features/terminal/zoom";
import {
  DEFAULT_TAB_SWIPE,
  TAB_SWIPE_SENSITIVITIES,
  type TabSwipePrefs,
  type TabSwipeSensitivity,
} from "../features/terminal/tabSwipe";
import type {
  KeyBinding,
  ShortcutAction,
  ShortcutOverrides,
} from "../features/terminal/shortcuts";
import { OVERRIDABLE_ACTIONS } from "../features/terminal/shortcuts";
import {
  AGENT_TINT_KEYS,
  DEFAULT_AGENT_TINTS,
  isHexColor,
  LEGACY_AGENT_TINTS,
  type AgentTintKey,
} from "../features/terminal/paneBackground";

export type ThemePreference = "system" | "dark" | "light";
export const THEME_PREFERENCES: readonly ThemePreference[] = ["system", "dark", "light"];

/**
 * 앱 종료 시 살아 있는 터미널 처리(04 §2의 창 닫기 정책과 짝):
 * ask = 매번 묻기, keep = 터미널을 백그라운드에 두고 앱만 종료,
 * terminate = 터미널까지 모두 종료.
 */
export type QuitBehavior = "ask" | "keep" | "terminate";
export const QUIT_BEHAVIORS: readonly QuitBehavior[] = ["ask", "keep", "terminate"];

/**
 * Shift+Space 한/영 강제 전환(iyagi 고유 기능, bridge/ime.rs):
 * auto = OS에 한국어 입력 소스가 있을 때만, on = 항상, off = Shift+Space는 공백.
 */
export type HangulToggleMode = "auto" | "on" | "off";
export const HANGUL_TOGGLE_MODES: readonly HangulToggleMode[] = ["auto", "on", "off"];

/**
 * Claude Code 실행의 모델 제공자(설정 → 연동 → Z.ai Coding Plan):
 * anthropic = Claude Code 자체 인증 그대로, zai-coding-plan = 데몬이 저장된
 * Z.ai 키로 Anthropic 호환 엔드포인트에 라우팅(LaunchRequest.claude_provider).
 * 값은 선택자일 뿐이다 — 키 자체는 프론트에 오지 않는다.
 */
export type ClaudeProviderPreference = "anthropic" | "zai-coding-plan";
export const CLAUDE_PROVIDER_PREFERENCES: readonly ClaudeProviderPreference[] = [
  "anthropic",
  "zai-coding-plan",
];

/**
 * Z.ai 라우팅 시 opus/sonnet 슬롯에 들어갈 주 모델 —
 * term-contracts `ZAI_CLAUDE_MAIN_MODELS`의 미러(데몬이 같은 목록으로 검증한다).
 */
export type ZaiMainModel = "glm-5.3[1m]" | "glm-5.3-flash[1m]";
export const ZAI_MAIN_MODELS: readonly ZaiMainModel[] = ["glm-5.3[1m]", "glm-5.3-flash[1m]"];
/** haiku/백그라운드 슬롯은 데몬이 고정한다(`ZAI_CLAUDE_HAIKU_MODEL`) — 표시용 미러. */
export const ZAI_HAIKU_MODEL: ZaiMainModel = "glm-5.3-flash[1m]";

/** 04-ui.md §4 "zoom 9..24px" — 기본 글꼴 크기도 같은 범위를 쓴다. */
export const MIN_BASE_FONT_SIZE = 9;
export const MAX_BASE_FONT_SIZE = 24;

export interface PreferenceValues {
  /** 화면 테마. system이면 OS의 prefers-color-scheme을 따른다. */
  theme: ThemePreference;
  claudeFullAutonomy: boolean;
  codexFullAutonomy: boolean;
  /**
   * Claude Code 터미널(빠른 실행·관리 실행·재개)을 어느 제공자로 열지.
   * 일반 셸에는 적용되지 않는다. 기본은 Claude Code 자체 인증.
   */
  claudeProvider: ClaudeProviderPreference;
  /** Z.ai 라우팅 시 주 모델(opus·sonnet 슬롯). claudeProvider가 zai-coding-plan일 때만 쓰인다. */
  zaiMainModel: ZaiMainModel;
  /** 새 터미널이 시작하는 글꼴 크기(pane zoom의 기준점이기도 하다). */
  baseFontSize: number;
  /** 창을 닫을 때 묻지 않고 터미널까지 종료(04 §2). */
  terminateOnClose: boolean;
  /** 앱 종료 시 터미널 처리 — 기본은 매번 묻기. */
  quitBehavior: QuitBehavior;
  /** 터미널 글꼴 패밀리(쉼표 폴백 목록, W3-1). */
  /**
   * null = 자동(xtermSetup의 리졸버가 내장 Nanum/설치된 한국어 코딩 폰트를
   * 고른다). 사용자가 입력하면 그 스택을 쓴다.
   */
  fontFamily: string | null;
  /** 커서 모양(W3-1). */
  cursorStyle: "block" | "underline" | "bar";
  /** Shift+Space 한/영 강제 전환 — 기본은 자동(한국어 입력 소스가 있을 때). */
  hangulToggle: HangulToggleMode;
  /** 스크롤백 줄 수(pane당, W3-1). */
  scrollbackLines: number;
  /** OSC 52 클립보드 쓰기 허용(기본 거절 — 04 §6, W2). */
  osc52Write: boolean;
  /**
   * WebGL 렌더러(기본 켬). 박스·블록 글자가 행 틈 없이 이어진다. GPU/드라이버
   * 문제가 있으면 끄면 xterm 기본 DOM 렌더러를 쓴다(04-ui §4).
   */
  gpuRenderer: boolean;
  /**
   * 에이전트별 터미널 배경색(paneBackground.ts). 감지된 에이전트마다
   * 서로 다른 색조로 칠한다. 기본 켬.
   */
  agentBackgrounds: boolean;
  /** 에이전트별(claude/zai/codex/opencode) 색조 색 "#rrggbb". */
  agentBackgroundColors: Record<AgentTintKey, string>;
  /** 탭 전환 시 들어오는 탭이 살짝 밀려 들어오는 효과(features/terminal/tabSwitchEffect.ts). */
  tabSwitchEffect: boolean;
  /** 저신뢰 텍스트 패턴 감지 배지(기본 off — §2.1 opt-in fallback, W3-5). */
  interventionTextBadge: boolean;
  /**
   * AI 작업 대화 입력창에서 Enter로 전송(05-ui §4 "Enter 전송은 사용자
   * 설정을 따른다"). 끄면 Ctrl/Cmd+Enter로만 전송한다. 기본 켬.
   */
  missionEnterSend: boolean;
  /** 단축키 재정의(W3-2): 바인딩 테이블만 소유, 가드는 불가침. */
  shortcutOverrides: ShortcutOverrides;
  /**
   * 트랙패드 두 손가락 가로 스와이프로 탭 전환(features/terminal/tabSwipe.ts).
   * 감도는 문턱(px), reverse는 자연스러운 스크롤을 끈 사용자용, wrap은 끝에서
   * 반대편 끝으로 순환할지.
   */
  tabSwipe: TabSwipePrefs;
}

export const CURSOR_STYLES: readonly string[] = ["block", "underline", "bar"];
/** 레거시 기본 스택 — 저장된 이 값은 '자동'으로 마이그레이션한다. */
export const LEGACY_DEFAULT_FONT_FAMILY = "Consolas, 'Courier New', monospace";
/** 스크롤백 상한 — Wave 1 메모리 매트릭스 실행 전의 보수적 상한(W3-1). */
export const MIN_SCROLLBACK_LINES = 100;
export const MAX_SCROLLBACK_LINES = 100_000;

export const DEFAULT_PREFERENCES: PreferenceValues = {
  theme: "system",
  claudeFullAutonomy: true,
  codexFullAutonomy: true,
  claudeProvider: "anthropic",
  zaiMainModel: "glm-5.3[1m]",
  baseFontSize: DEFAULT_FONT_SIZE,
  terminateOnClose: false,
  quitBehavior: "ask",
  fontFamily: null,
  cursorStyle: "block",
  hangulToggle: "auto",
  scrollbackLines: 2000,
  osc52Write: false,
  gpuRenderer: true,
  agentBackgrounds: true,
  agentBackgroundColors: { ...DEFAULT_AGENT_TINTS },
  tabSwitchEffect: true,
  interventionTextBadge: false,
  missionEnterSend: true,
  shortcutOverrides: {},
  tabSwipe: DEFAULT_TAB_SWIPE,
};

export function clampBaseFontSize(size: number): number {
  if (!Number.isFinite(size)) return DEFAULT_PREFERENCES.baseFontSize;
  return Math.min(MAX_BASE_FONT_SIZE, Math.max(MIN_BASE_FONT_SIZE, Math.round(size)));
}

export function clampScrollbackLines(size: number): number {
  if (!Number.isFinite(size)) return DEFAULT_PREFERENCES.scrollbackLines;
  return Math.min(MAX_SCROLLBACK_LINES, Math.max(MIN_SCROLLBACK_LINES, Math.round(size)));
}

/** 글꼴 패밀리 입력 정리: null(자동) 허용, 빈 값도 자동, 상한 200자. */
export function sanitizeFontFamily(raw: unknown): string | null {
  if (raw === null) return null;
  if (typeof raw !== "string") return null;
  const family = raw.trim();
  if (family.length === 0 || family.length > 200) return null;
  // 레거시 기본 스택이 저장돼 있으면 사용자가 고른 값이 아니라 기본값이
  // 굳은 것(설정 저장 시점에 함께 persist됨) — 자동 선택으로 되돌린다.
  // 따옴표·공백 변형도 잡는다(내장 한글 폰트 계약 복원).
  if (normalizeFontStack(family) === normalizeFontStack(LEGACY_DEFAULT_FONT_FAMILY)) return null;
  return family;
}

/** 폰트 스택 비교 정규화: 인용부호·공백 제거 후 소문자화. */
function normalizeFontStack(stack: string): string {
  return stack.replace(/['"\s]/g, "").toLowerCase();
}

/** 저장된 값 → 유효한 설정. 모르는 값·타입은 기본값으로 떨어뜨린다. */
export function sanitizePreferences(raw: unknown): PreferenceValues {
  const value = (typeof raw === "object" && raw !== null ? raw : {}) as Partial<
    Record<keyof PreferenceValues, unknown>
  >;
  return {
    theme: THEME_PREFERENCES.includes(value.theme as ThemePreference)
      ? (value.theme as ThemePreference)
      : DEFAULT_PREFERENCES.theme,
    baseFontSize:
      typeof value.baseFontSize === "number"
        ? clampBaseFontSize(value.baseFontSize)
        : DEFAULT_PREFERENCES.baseFontSize,
    terminateOnClose:
      typeof value.terminateOnClose === "boolean"
        ? value.terminateOnClose
        : DEFAULT_PREFERENCES.terminateOnClose,
    quitBehavior: QUIT_BEHAVIORS.includes(value.quitBehavior as QuitBehavior)
      ? (value.quitBehavior as QuitBehavior)
      : DEFAULT_PREFERENCES.quitBehavior,
    claudeFullAutonomy: typeof value.claudeFullAutonomy === "boolean" ? value.claudeFullAutonomy : DEFAULT_PREFERENCES.claudeFullAutonomy,
    codexFullAutonomy: typeof value.codexFullAutonomy === "boolean" ? value.codexFullAutonomy : DEFAULT_PREFERENCES.codexFullAutonomy,
    claudeProvider: CLAUDE_PROVIDER_PREFERENCES.includes(value.claudeProvider as ClaudeProviderPreference)
      ? (value.claudeProvider as ClaudeProviderPreference)
      : DEFAULT_PREFERENCES.claudeProvider,
    zaiMainModel: ZAI_MAIN_MODELS.includes(value.zaiMainModel as ZaiMainModel)
      ? (value.zaiMainModel as ZaiMainModel)
      : DEFAULT_PREFERENCES.zaiMainModel,
    fontFamily: sanitizeFontFamily(value.fontFamily),
    cursorStyle: CURSOR_STYLES.includes(value.cursorStyle as string)
      ? (value.cursorStyle as PreferenceValues["cursorStyle"])
      : DEFAULT_PREFERENCES.cursorStyle,
    hangulToggle: HANGUL_TOGGLE_MODES.includes(value.hangulToggle as HangulToggleMode)
      ? (value.hangulToggle as HangulToggleMode)
      : DEFAULT_PREFERENCES.hangulToggle,
    scrollbackLines:
      typeof value.scrollbackLines === "number"
        ? clampScrollbackLines(value.scrollbackLines)
        : DEFAULT_PREFERENCES.scrollbackLines,
    osc52Write:
      typeof value.osc52Write === "boolean" ? value.osc52Write : DEFAULT_PREFERENCES.osc52Write,
    gpuRenderer:
      typeof value.gpuRenderer === "boolean" ? value.gpuRenderer : DEFAULT_PREFERENCES.gpuRenderer,
    agentBackgrounds:
      typeof value.agentBackgrounds === "boolean"
        ? value.agentBackgrounds
        : DEFAULT_PREFERENCES.agentBackgrounds,
    agentBackgroundColors: sanitizeAgentBackgroundColors(value.agentBackgroundColors),
    tabSwitchEffect:
      typeof value.tabSwitchEffect === "boolean"
        ? value.tabSwitchEffect
        : DEFAULT_PREFERENCES.tabSwitchEffect,
    interventionTextBadge:
      typeof value.interventionTextBadge === "boolean"
        ? value.interventionTextBadge
        : DEFAULT_PREFERENCES.interventionTextBadge,
    missionEnterSend:
      typeof value.missionEnterSend === "boolean"
        ? value.missionEnterSend
        : DEFAULT_PREFERENCES.missionEnterSend,
    shortcutOverrides: sanitizeShortcutOverrides(value.shortcutOverrides),
    tabSwipe: sanitizeTabSwipe(value.tabSwipe),
  };
}

/**
 * 트랙패드 제스처 설정 정리. 이 항목이 없던 시절에 저장된 값(undefined)도,
 * 일부만 있는 값도 모자란 자리만 기본값으로 채운다 — 설정 하나가 늘었다고
 * 사용자의 다른 트랙패드 선택이 사라지면 안 된다.
 */
export function sanitizeTabSwipe(raw: unknown): TabSwipePrefs {
  if (typeof raw !== "object" || raw === null) return DEFAULT_TAB_SWIPE;
  const value = raw as Partial<Record<keyof TabSwipePrefs, unknown>>;
  return {
    enabled: typeof value.enabled === "boolean" ? value.enabled : DEFAULT_TAB_SWIPE.enabled,
    sensitivity: TAB_SWIPE_SENSITIVITIES.includes(value.sensitivity as TabSwipeSensitivity)
      ? (value.sensitivity as TabSwipeSensitivity)
      : DEFAULT_TAB_SWIPE.sensitivity,
    reverse: typeof value.reverse === "boolean" ? value.reverse : DEFAULT_TAB_SWIPE.reverse,
    wrap: typeof value.wrap === "boolean" ? value.wrap : DEFAULT_TAB_SWIPE.wrap,
  };
}

/** 재정의 구조만 검증(예약·충돌은 플랫폼을 아는 설정 UI에서). */
function sanitizeShortcutOverrides(raw: unknown): ShortcutOverrides {
  if (typeof raw !== "object" || raw === null) return {};
  const result: ShortcutOverrides = {};
  for (const [action, value] of Object.entries(raw as Record<string, unknown>)) {
    if (!(OVERRIDABLE_ACTIONS as readonly string[]).includes(action)) continue;
    if (value === "pass") {
      result[action as ShortcutAction] = "pass";
      continue;
    }
    if (
      typeof value === "object" &&
      value !== null &&
      typeof (value as KeyBinding).code === "string" &&
      (value as KeyBinding).code.length > 0 &&
      (value as KeyBinding).code.length <= 16 &&
      typeof (value as KeyBinding).ctrl === "boolean" &&
      typeof (value as KeyBinding).meta === "boolean" &&
      typeof (value as KeyBinding).shift === "boolean"
    ) {
      const binding = value as KeyBinding;
      result[action as ShortcutAction] = {
        code: binding.code,
        ctrl: binding.ctrl,
        meta: binding.meta,
        shift: binding.shift,
      };
    }
  }
  return result;
}

/**
 * 에이전트별 tint 색 정리 — 형식이 어긋난 항목은 기본값으로 되돌리고,
 * 옛 기본값 그대로 저장된 항목도 새 기본값으로 옮긴다(LEGACY_AGENT_TINTS).
 *
 * 뒤쪽이 없으면 기본색을 바꿔도 이미 앱을 켜 본 사용자에게는 반영되지
 * 않는다 — 첫 실행 때 그때의 기본값이 localStorage에 그대로 저장되고,
 * 저장값이 언제나 기본값을 이기기 때문이다. 직접 고른 색은 그대로 둔다
 * (옛 기본값과 똑같은 색을 일부러 고른 경우만 함께 옮겨진다).
 */
function sanitizeAgentBackgroundColors(raw: unknown): Record<AgentTintKey, string> {
  const out = { ...DEFAULT_PREFERENCES.agentBackgroundColors };
  if (typeof raw === "object" && raw !== null) {
    for (const key of AGENT_TINT_KEYS) {
      const value = (raw as Record<string, unknown>)[key];
      if (!isHexColor(value)) continue;
      const color = value.toLowerCase();
      if (LEGACY_AGENT_TINTS[key].includes(color)) continue;
      out[key] = color;
    }
  }
  return out;
}

export interface PreferencesState extends PreferenceValues {
  setTheme(theme: ThemePreference): void;
  setCliAutonomy(kind: "claude" | "codex", enabled: boolean): void;
  /** Claude Code 제공자 — 모르는 값은 Claude Code 자체 인증으로 되돌린다. */
  setClaudeProvider(provider: ClaudeProviderPreference): void;
  /** Z.ai 라우팅 주 모델 — 목록 밖 값은 기본 모델로 되돌린다. */
  setZaiMainModel(model: ZaiMainModel): void;
  setBaseFontSize(size: number): void;
  setTerminateOnClose(value: boolean): void;
  setQuitBehavior(value: QuitBehavior): void;
  setFontFamily(family: string): void;
  setCursorStyle(style: PreferenceValues["cursorStyle"]): void;
  setHangulToggle(mode: HangulToggleMode): void;
  setScrollbackLines(lines: number): void;
  setOsc52Write(value: boolean): void;
  setGpuRenderer(value: boolean): void;
  /** 에이전트별 배경 tint 켜기/끄기(paneBackground.ts). */
  setAgentBackgrounds(enabled: boolean): void;
  /** 에이전트별 tint 색 — 형식이 어긋나면 무시한다. */
  setAgentBackgroundColor(key: AgentTintKey, color: string): void;
  setTabSwitchEffect(value: boolean): void;
  setInterventionTextBadge(value: boolean): void;
  setShortcutOverride(action: ShortcutAction, binding: KeyBinding | "pass" | null): void;
  /** 트랙패드 제스처 설정 — 바꾼 항목만 넘긴다(나머지는 그대로). */
  setTabSwipe(patch: Partial<TabSwipePrefs>): void;
  /** 한 항목만 기본값으로(설정 행의 "기본값으로" 버튼). */
  resetPreference(key: keyof PreferenceValues): void;
  /** 이 스토어가 가진 값 전부를 기본값으로. 프로필·워크스페이스는 건드리지 않는다. */
  resetAll(): void;
}

/** 항목이 기본값에서 벗어났는지 — 설정 UI의 "변경됨" 배지·되돌리기 노출 조건. */
export function isPreferenceChanged(state: PreferenceValues, key: keyof PreferenceValues): boolean {
  return state[key] !== DEFAULT_PREFERENCES[key];
}

const safeLocalStorage = {
  getItem: (key: string): string | null => {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  },
  setItem: (key: string, value: string): void => {
    try {
      localStorage.setItem(key, value);
    } catch {
      /* Storage may be disabled. */
    }
  },
  removeItem: (key: string): void => {
    try {
      localStorage.removeItem(key);
    } catch {
      /* Storage may be disabled. */
    }
  },
};

export const usePreferences = create<PreferencesState>()(
  persist(
    (set) => ({
      ...DEFAULT_PREFERENCES,
      setTheme: (theme) => set({ theme }),
      setCliAutonomy: (kind, enabled) => set(kind === "claude" ? { claudeFullAutonomy: enabled } : { codexFullAutonomy: enabled }),
      setClaudeProvider: (provider) =>
        set({
          claudeProvider: CLAUDE_PROVIDER_PREFERENCES.includes(provider)
            ? provider
            : DEFAULT_PREFERENCES.claudeProvider,
        }),
      setZaiMainModel: (model) =>
        set({ zaiMainModel: ZAI_MAIN_MODELS.includes(model) ? model : DEFAULT_PREFERENCES.zaiMainModel }),
      setBaseFontSize: (size) => set({ baseFontSize: clampBaseFontSize(size) }),
      setTerminateOnClose: (terminateOnClose) => set({ terminateOnClose }),
      setQuitBehavior: (quitBehavior) =>
        set({ quitBehavior: QUIT_BEHAVIORS.includes(quitBehavior) ? quitBehavior : "ask" }),
      setFontFamily: (family) => set({ fontFamily: sanitizeFontFamily(family) }),
      setCursorStyle: (cursorStyle) => set({ cursorStyle }),
      setHangulToggle: (hangulToggle) =>
        set({ hangulToggle: HANGUL_TOGGLE_MODES.includes(hangulToggle) ? hangulToggle : "auto" }),
      setScrollbackLines: (lines) => set({ scrollbackLines: clampScrollbackLines(lines) }),
      setOsc52Write: (osc52Write) => set({ osc52Write }),
      setGpuRenderer: (gpuRenderer) => set({ gpuRenderer }),
      setTabSwitchEffect: (tabSwitchEffect) => set({ tabSwitchEffect }),
      setInterventionTextBadge: (interventionTextBadge) => set({ interventionTextBadge }),
      setShortcutOverride: (action, binding) =>
        set((state) => {
          const next = { ...state.shortcutOverrides };
          if (binding === null) delete next[action];
          else next[action] = binding;
          return { shortcutOverrides: next };
        }),
      setTabSwipe: (patch) =>
        set((state) => ({ tabSwipe: sanitizeTabSwipe({ ...state.tabSwipe, ...patch }) })),
      setAgentBackgrounds: (agentBackgrounds) => set({ agentBackgrounds }),
      setAgentBackgroundColor: (key, color) =>
        set((state) => ({
          agentBackgroundColors: {
            ...state.agentBackgroundColors,
            [key]: isHexColor(color) ? color.toLowerCase() : state.agentBackgroundColors[key],
          },
        })),
      resetPreference: (key) => set({ [key]: DEFAULT_PREFERENCES[key] } as Partial<PreferenceValues>),
      resetAll: () => set({ ...DEFAULT_PREFERENCES }),
    }),
    {
      // 예전 이름 유지: 이미 저장된 terminateOnClose를 그대로 이어받는다.
      name: "iyagi.terminal-preferences.v1",
      partialize: (state): PreferenceValues => ({
        theme: state.theme,
        claudeFullAutonomy: state.claudeFullAutonomy,
        codexFullAutonomy: state.codexFullAutonomy,
        claudeProvider: state.claudeProvider,
        zaiMainModel: state.zaiMainModel,
        baseFontSize: state.baseFontSize,
        terminateOnClose: state.terminateOnClose,
        quitBehavior: state.quitBehavior,
        fontFamily: state.fontFamily,
        cursorStyle: state.cursorStyle,
        hangulToggle: state.hangulToggle,
        scrollbackLines: state.scrollbackLines,
        osc52Write: state.osc52Write,
        gpuRenderer: state.gpuRenderer,
        agentBackgrounds: state.agentBackgrounds,
        agentBackgroundColors: state.agentBackgroundColors,
        tabSwitchEffect: state.tabSwitchEffect,
        interventionTextBadge: state.interventionTextBadge,
        missionEnterSend: state.missionEnterSend,
        shortcutOverrides: state.shortcutOverrides,
        tabSwipe: state.tabSwipe,
      }),
      merge: (persisted, current) => ({ ...current, ...sanitizePreferences(persisted) }),
      storage: createJSONStorage(() => safeLocalStorage),
    },
  ),
);
