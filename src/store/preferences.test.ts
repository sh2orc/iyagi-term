/**
 * 설정 스토어 계약: 값 정상화(잘못 저장된 값은 조용히 기본값), 항목 단위
 * 되돌리기, 지속화.
 */

import { afterEach, describe, expect, it } from "vitest";
import { LEGACY_DEFAULT_FONT_FAMILY, sanitizeFontFamily } from "./preferences";
import { createJSONStorage } from "zustand/middleware";
import { LEGACY_AGENT_TINTS } from "../features/terminal/paneBackground";
import {
  DEFAULT_PREFERENCES,
  MAX_BASE_FONT_SIZE,
  MIN_BASE_FONT_SIZE,
  clampBaseFontSize,
  isPreferenceChanged,
  sanitizePreferences,
  sanitizeTabSwipe,
  usePreferences,
} from "./preferences";

describe("clampBaseFontSize", () => {
  it("keeps sizes inside the spec range and rounds", () => {
    expect(clampBaseFontSize(MIN_BASE_FONT_SIZE - 5)).toBe(MIN_BASE_FONT_SIZE);
    expect(clampBaseFontSize(MAX_BASE_FONT_SIZE + 5)).toBe(MAX_BASE_FONT_SIZE);
    expect(clampBaseFontSize(13.6)).toBe(14);
  });

  it("falls back to the default for non-finite input", () => {
    expect(clampBaseFontSize(Number.NaN)).toBe(DEFAULT_PREFERENCES.baseFontSize);
  });
});

describe("sanitizePreferences", () => {
  it("accepts a valid payload untouched", () => {
    expect(
      sanitizePreferences({
        theme: "light",
        claudeFullAutonomy: false,
        codexFullAutonomy: true,
        claudeProvider: "zai-coding-plan",
        zaiMainModel: "glm-5.3-flash[1m]",
        baseFontSize: 16,
        terminateOnClose: true,
        quitBehavior: "keep",
        fontFamily: "Menlo, monospace",
        cursorStyle: "underline",
        hangulToggle: "on",
        scrollbackLines: 5000,
        osc52Write: true,
        gpuRenderer: false,
        tabSwitchEffect: false,
        interventionTextBadge: false,
        missionEnterSend: true,
        shortcutOverrides: {},
        tabSwipe: { enabled: false, sensitivity: "high", reverse: true, wrap: true },
      }),
    ).toEqual({
      theme: "light",
      claudeFullAutonomy: false,
      codexFullAutonomy: true,
      claudeProvider: "zai-coding-plan",
      zaiMainModel: "glm-5.3-flash[1m]",
      baseFontSize: 16,
      terminateOnClose: true,
      quitBehavior: "keep",
      fontFamily: "Menlo, monospace",
      cursorStyle: "underline",
      hangulToggle: "on",
      scrollbackLines: 5000,
      osc52Write: true,
      gpuRenderer: false,
      tabSwitchEffect: false,
      interventionTextBadge: false,
      missionEnterSend: true,
      shortcutOverrides: {},
      tabSwipe: { enabled: false, sensitivity: "high", reverse: true, wrap: true },
      agentBackgrounds: true,
      agentBackgroundColors: DEFAULT_PREFERENCES.agentBackgroundColors,
    });
  });

  it("drops unknown themes, bad types and out-of-range sizes", () => {
    expect(sanitizePreferences({ theme: "solarized", baseFontSize: "big", terminateOnClose: 1 })).toEqual(
      DEFAULT_PREFERENCES,
    );
    expect(sanitizePreferences({ baseFontSize: 999 }).baseFontSize).toBe(MAX_BASE_FONT_SIZE);
  });

  it("종료 시 터미널 처리는 ask/keep/terminate만 받고 나머지는 묻기로 되돌린다", () => {
    expect(sanitizePreferences({ quitBehavior: "terminate" }).quitBehavior).toBe("terminate");
    expect(sanitizePreferences({ quitBehavior: "keep" }).quitBehavior).toBe("keep");
    expect(sanitizePreferences({ quitBehavior: "yes" }).quitBehavior).toBe("ask");
    expect(sanitizePreferences({ quitBehavior: true }).quitBehavior).toBe("ask");
    expect(sanitizePreferences({}).quitBehavior).toBe("ask");
  });

  it("treats a missing or non-object payload as defaults (첫 실행·손상 저장)", () => {
    expect(sanitizePreferences(null)).toEqual(DEFAULT_PREFERENCES);
    expect(sanitizePreferences("nope")).toEqual(DEFAULT_PREFERENCES);
  });

  it("옛 기본 tint로 저장된 색은 새 기본값으로 옮기고 직접 고른 색은 그대로 둔다", () => {
    const { agentBackgroundColors } = sanitizePreferences({
      agentBackgroundColors: {
        claude: LEGACY_AGENT_TINTS.claude[0],
        zai: LEGACY_AGENT_TINTS.zai[0],
        codex: "#0000ff",
      },
    });
    // 옛 기본값 = 사용자가 고른 적 없는 값 → 새 기본값으로.
    expect(agentBackgroundColors.claude).toBe(DEFAULT_PREFERENCES.agentBackgroundColors.claude);
    expect(agentBackgroundColors.claude).not.toBe(LEGACY_AGENT_TINTS.claude[0]);
    expect(agentBackgroundColors.zai).toBe(DEFAULT_PREFERENCES.agentBackgroundColors.zai);
    // 직접 고른 색은 지킨다.
    expect(agentBackgroundColors.codex).toBe("#0000ff");
    // 빠진 항목도 기본값.
    expect(agentBackgroundColors.opencode).toBe(DEFAULT_PREFERENCES.agentBackgroundColors.opencode);
  });
});

describe("preferences store", () => {
  afterEach(() => usePreferences.setState({ ...DEFAULT_PREFERENCES }));

  it("clamps through the setter and reports changed items", () => {
    usePreferences.getState().setBaseFontSize(200);
    expect(usePreferences.getState().baseFontSize).toBe(MAX_BASE_FONT_SIZE);
    expect(isPreferenceChanged(usePreferences.getState(), "baseFontSize")).toBe(true);
    expect(isPreferenceChanged(usePreferences.getState(), "theme")).toBe(false);
  });

  it("resets one item without disturbing the others", () => {
    usePreferences.getState().setTheme("light");
    usePreferences.getState().setTerminateOnClose(true);
    usePreferences.getState().resetPreference("theme");
    expect(usePreferences.getState().theme).toBe(DEFAULT_PREFERENCES.theme);
    expect(usePreferences.getState().terminateOnClose).toBe(true);
  });

  it("resets everything at once", () => {
    usePreferences.getState().setTheme("dark");
    usePreferences.getState().setBaseFontSize(20);
    usePreferences.getState().resetAll();
    expect({ ...usePreferences.getState() }).toMatchObject(DEFAULT_PREFERENCES);
  });

  it("persists and rehydrates every value", async () => {
    const values = new Map<string, string>();
    usePreferences.persist.setOptions({
      storage: createJSONStorage(() => ({
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => { values.set(key, value); },
        removeItem: (key: string) => { values.delete(key); },
      })),
    });
    usePreferences.getState().setTheme("light");
    usePreferences.getState().setBaseFontSize(18);
    usePreferences.getState().setTerminateOnClose(true);
    usePreferences.getState().setQuitBehavior("terminate");
    usePreferences.getState().setFontFamily("Menlo, monospace");
    usePreferences.getState().setCursorStyle("bar");
    usePreferences.getState().setHangulToggle("on");
    usePreferences.getState().setScrollbackLines(4096);
    usePreferences.getState().setOsc52Write(true);
    usePreferences.getState().setInterventionTextBadge(true);
    usePreferences.getState().setCliAutonomy("claude", false);
    usePreferences.getState().setCliAutonomy("codex", false);
    usePreferences.getState().setClaudeProvider("zai-coding-plan");
    usePreferences.getState().setZaiMainModel("glm-5.3-flash[1m]");
    usePreferences.getState().setShortcutOverride("paste", { code: "KeyG", ctrl: true, meta: false, shift: true });
    usePreferences.getState().setTabSwipe({ sensitivity: "high", wrap: true });
    expect(JSON.parse([...values.values()][0]).state).toEqual({
      claudeFullAutonomy: false,
      codexFullAutonomy: false,
      claudeProvider: "zai-coding-plan",
      zaiMainModel: "glm-5.3-flash[1m]",
      agentBackgrounds: true,
      agentBackgroundColors: {
        claude: "#e0521f",
        codex: "#06a86b",
        opencode: "#1f6fe8",
        zai: "#6d3df5",
      },
      theme: "light",
      baseFontSize: 18,
      terminateOnClose: true,
      quitBehavior: "terminate",
      fontFamily: "Menlo, monospace",
      cursorStyle: "bar",
      hangulToggle: "on",
      scrollbackLines: 4096,
      osc52Write: true,
      gpuRenderer: true,
      tabSwitchEffect: true,
      interventionTextBadge: true,
      missionEnterSend: true,
      shortcutOverrides: { paste: { code: "KeyG", ctrl: true, meta: false, shift: true } },
      tabSwipe: { enabled: true, sensitivity: "high", reverse: false, wrap: true },
    });
    const key = [...values.keys()][0];
    const saved = values.get(key)!;
    usePreferences.setState({
      claudeFullAutonomy: true,
      codexFullAutonomy: true,
      claudeProvider: "anthropic",
      zaiMainModel: "glm-5.3[1m]",
    });
    values.set(key, saved);
    await usePreferences.persist.rehydrate();
    expect(usePreferences.getState().claudeFullAutonomy).toBe(false);
    expect(usePreferences.getState().codexFullAutonomy).toBe(false);
    expect(usePreferences.getState().claudeProvider).toBe("zai-coding-plan");
    expect(usePreferences.getState().zaiMainModel).toBe("glm-5.3-flash[1m]");
    expect(usePreferences.getState().theme).toBe("light");
    expect(usePreferences.getState().baseFontSize).toBe(18);
    expect(usePreferences.getState().terminateOnClose).toBe(true);
    expect(usePreferences.getState().quitBehavior).toBe("terminate");
    expect(usePreferences.getState().tabSwipe).toEqual({
      enabled: true,
      sensitivity: "high",
      reverse: false,
      wrap: true,
    });
  });
});


describe("autonomy preference defaults", () => {
  it("defaults existing installations to on while retaining explicit unchecked values", () => {
    expect(sanitizePreferences({}).claudeFullAutonomy).toBe(true);
    expect(sanitizePreferences({}).codexFullAutonomy).toBe(true);
    expect(sanitizePreferences({ claudeFullAutonomy: false, codexFullAutonomy: true })).toMatchObject({ claudeFullAutonomy: false, codexFullAutonomy: true });
    expect(sanitizePreferences({ claudeFullAutonomy: "false", codexFullAutonomy: 0 })).toMatchObject({ claudeFullAutonomy: true, codexFullAutonomy: true });
  });
});


describe("Claude 제공자 라우팅(claudeProvider · zaiMainModel)", () => {
  afterEach(() => usePreferences.setState({ ...DEFAULT_PREFERENCES }));

  it("기본은 Claude Code 자체 인증 + GLM-5.3 — 이 설정이 없던 저장값도 그렇다", () => {
    expect(DEFAULT_PREFERENCES.claudeProvider).toBe("anthropic");
    expect(DEFAULT_PREFERENCES.zaiMainModel).toBe("glm-5.3[1m]");
    expect(sanitizePreferences({ theme: "dark" })).toMatchObject({
      claudeProvider: "anthropic",
      zaiMainModel: "glm-5.3[1m]",
    });
  });

  it("알려진 값만 받고 나머지는 조용히 기본값으로 떨어진다", () => {
    expect(sanitizePreferences({ claudeProvider: "zai-coding-plan", zaiMainModel: "glm-5.3-flash[1m]" })).toMatchObject({
      claudeProvider: "zai-coding-plan",
      zaiMainModel: "glm-5.3-flash[1m]",
    });
    // 데몬이 거부할 모델 id(계약 ZAI_CLAUDE_MAIN_MODELS 밖)는 저장값이어도 받지 않는다.
    expect(sanitizePreferences({ claudeProvider: "openai", zaiMainModel: "glm-4.5" })).toMatchObject({
      claudeProvider: "anthropic",
      zaiMainModel: "glm-5.3[1m]",
    });
    expect(sanitizePreferences({ claudeProvider: true, zaiMainModel: 1 })).toMatchObject({
      claudeProvider: "anthropic",
      zaiMainModel: "glm-5.3[1m]",
    });
  });

  it("setter도 목록 밖 값은 기본값으로 되돌리고, 항목 되돌리기가 된다", () => {
    usePreferences.getState().setClaudeProvider("zai-coding-plan");
    usePreferences.getState().setZaiMainModel("glm-5.3-flash[1m]");
    expect(usePreferences.getState().claudeProvider).toBe("zai-coding-plan");
    expect(usePreferences.getState().zaiMainModel).toBe("glm-5.3-flash[1m]");
    expect(isPreferenceChanged(usePreferences.getState(), "claudeProvider")).toBe(true);
    usePreferences.getState().setZaiMainModel("glm-9" as never);
    expect(usePreferences.getState().zaiMainModel).toBe("glm-5.3[1m]");
    usePreferences.getState().setClaudeProvider("bedrock" as never);
    expect(usePreferences.getState().claudeProvider).toBe("anthropic");
    usePreferences.getState().setClaudeProvider("zai-coding-plan");
    usePreferences.getState().resetPreference("claudeProvider");
    expect(usePreferences.getState().claudeProvider).toBe("anthropic");
  });
});

describe("fontFamily — null(자동)과 레거시 마이그레이션", () => {
  it("기본값은 null(자동)이다", () => {
    expect(sanitizeFontFamily(undefined)).toBeNull();
    expect(sanitizeFontFamily(null)).toBeNull();
    expect(sanitizeFontFamily("")).toBeNull();
  });

  it("레거시 기본 스택이 저장돼 있으면 자동으로 마이그레이션된다", () => {
    expect(sanitizeFontFamily(LEGACY_DEFAULT_FONT_FAMILY)).toBeNull();
    // 따옴표·공백 변형도 잡는다.
    expect(sanitizeFontFamily("Consolas,'Courier New',monospace")).toBeNull();
    expect(sanitizeFontFamily(" consolas , 'courier new' , monospace ")).toBeNull();
  });

  it("사용자가 고른 스택은 그대로 쓴다", () => {
    expect(sanitizeFontFamily("Menlo, monospace")).toBe("Menlo, monospace");
  });
});

describe("트랙패드 제스처 설정(tabSwipe)", () => {
  afterEach(() => usePreferences.setState({ ...DEFAULT_PREFERENCES }));

  it("기본값은 켬·보통·정방향·순환 없음이다", () => {
    expect(DEFAULT_PREFERENCES.tabSwipe).toEqual({
      enabled: true,
      sensitivity: "medium",
      reverse: false,
      wrap: false,
    });
  });

  it("이 설정이 없던 시절에 저장된 값도 기본값으로 살아난다", () => {
    expect(sanitizePreferences({ theme: "dark" }).tabSwipe).toEqual(DEFAULT_PREFERENCES.tabSwipe);
    expect(sanitizeTabSwipe(undefined)).toEqual(DEFAULT_PREFERENCES.tabSwipe);
    expect(sanitizeTabSwipe(null)).toEqual(DEFAULT_PREFERENCES.tabSwipe);
    expect(sanitizeTabSwipe("nope")).toEqual(DEFAULT_PREFERENCES.tabSwipe);
  });

  it("일부만 저장된 값은 모자란 자리만 채운다(다른 선택을 지우지 않는다)", () => {
    expect(sanitizeTabSwipe({ enabled: false })).toEqual({
      enabled: false,
      sensitivity: "medium",
      reverse: false,
      wrap: false,
    });
    expect(sanitizeTabSwipe({ sensitivity: "low", wrap: true })).toEqual({
      enabled: true,
      sensitivity: "low",
      reverse: false,
      wrap: true,
    });
  });

  it("모르는 감도·타입은 조용히 기본값으로 떨어진다", () => {
    expect(sanitizeTabSwipe({ sensitivity: "turbo" }).sensitivity).toBe("medium");
    expect(sanitizeTabSwipe({ enabled: 1, reverse: "yes", wrap: null })).toEqual(
      DEFAULT_PREFERENCES.tabSwipe,
    );
  });

  it("setTabSwipe는 넘긴 항목만 바꾸고, 되돌리기는 한 번에 기본값으로", () => {
    usePreferences.getState().setTabSwipe({ sensitivity: "high" });
    usePreferences.getState().setTabSwipe({ reverse: true });
    expect(usePreferences.getState().tabSwipe).toEqual({
      enabled: true,
      sensitivity: "high",
      reverse: true,
      wrap: false,
    });
    usePreferences.getState().resetPreference("tabSwipe");
    expect(usePreferences.getState().tabSwipe).toEqual(DEFAULT_PREFERENCES.tabSwipe);
  });
});
