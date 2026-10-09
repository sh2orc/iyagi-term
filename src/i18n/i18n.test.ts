/**
 * i18n 코어 계약: {name} 치환, 키 폴백, 언어 저장소 스키마 거부,
 * 그리고 섹션별 ko/en 키 집합 동일성(번역 누락 방지).
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_LANGUAGE,
  createI18nStore,
  detectLanguage,
  formatMessage,
  translate,
  useI18nStore,
} from "./core";
import { appSection } from "./sections/app";
import { settingsSection } from "./sections/settings";
import { terminalSection } from "./sections/terminal";
import { monitorSection } from "./sections/monitor";
import { workloadsSection } from "./sections/workloads";
import { profilesSection } from "./sections/profiles";
import { missionsSection } from "./sections/missions";

describe("formatMessage", () => {
  it("substitutes {name} placeholders", () => {
    expect(formatMessage("탭 {index}", { index: 3 })).toBe("탭 3");
    expect(formatMessage("{a}·{b}", { a: "x", b: 42 })).toBe("x·42");
  });

  it("leaves unknown placeholders untouched", () => {
    expect(formatMessage("WSL — {distro}", {})).toBe("WSL — {distro}");
    expect(formatMessage("no params")).toBe("no params");
  });
});

describe("translate", () => {
  it("falls back to the default language, then the key itself", () => {
    expect(translate("en", "__missing_everywhere__").startsWith("__missing_everywhere__")).toBe(
      true,
    );
  });

  it("translates registered keys per language", () => {
    // index.ts 등록 전에는 섹션 함수로 직접 검증한다(테스트는 core만 import).
    expect(appSection.ko["app.empty.title"]).toBe("빈 프로젝트");
    expect(appSection.en["app.empty.title"]).toBe("Empty project");
  });
});

describe("detectLanguage", () => {
  it("returns the default language when navigator is absent (node)", () => {
    expect(detectLanguage()).toBe(DEFAULT_LANGUAGE);
  });

  it("maps ko* → ko, anything else → en (브라우저 환경 흉내)", () => {
    // window까지 붙여야 브라우저 경로로 들어간다(node에선 window가 없다).
    vi.stubGlobal("window", {});
    vi.stubGlobal("navigator", { language: "ko-KR" });
    expect(detectLanguage()).toBe("ko");
    vi.stubGlobal("navigator", { language: "en-US" });
    expect(detectLanguage()).toBe("en");
    vi.unstubAllGlobals();
  });
});

describe("ko/en key parity (번역 누락 방지)", () => {
  const sections: Record<string, { ko: Record<string, string>; en: Record<string, string> }> = {
    app: appSection,
    settings: settingsSection,
    terminal: terminalSection,
    monitor: monitorSection,
    workloads: workloadsSection,
    profiles: profilesSection,
    missions: missionsSection,
  };

  it.each(Object.keys(sections))("%s 섹션의 ko/en 키 집합이 동일하다", (name) => {
    const { ko, en } = sections[name];
    expect(Object.keys(en).sort()).toEqual(Object.keys(ko).sort());
  });

  it("ko 사전에 빈 문구가 없다", () => {
    for (const { ko } of Object.values(sections)) {
      for (const [key, value] of Object.entries(ko)) {
        expect(value.length, `${key} must not be empty`).toBeGreaterThan(0);
      }
    }
  });
});

describe("i18n store", () => {
  beforeEach(() => {
    useI18nStore.setState({ language: null });
  });

  it("persists an explicit language choice and resets to auto with null", () => {
    useI18nStore.getState().setLanguage("en");
    expect(useI18nStore.getState().language).toBe("en");
    useI18nStore.getState().setLanguage(null);
    expect(useI18nStore.getState().language).toBeNull();
  });

  it("rejects persisted schemas with unknown values (merge 가드)", async () => {
    const storage = new Map<string, string>([
      // persist가 쓰는 봉투 형식(version 포함)을 그대로 흉내 낸다.
      ["iyagi.language.v1", JSON.stringify({ state: { language: "fr" }, version: 0 })],
    ]);
    const store = createI18nStore({
      getItem: (k) => storage.get(k) ?? null,
      setItem: (k, v) => void storage.set(k, v),
      removeItem: (k) => void storage.delete(k),
    });
    await vi.waitFor(() => {
      expect(store.getState().language).toBeNull();
    });
  });

  it("accepts a valid persisted language", async () => {
    const storage = new Map<string, string>([
      ["iyagi.language.v1", JSON.stringify({ state: { language: "en" }, version: 0 })],
    ]);
    const store = createI18nStore({
      getItem: (k) => storage.get(k) ?? null,
      setItem: (k, v) => void storage.set(k, v),
      removeItem: (k) => void storage.delete(k),
    });
    await vi.waitFor(() => {
      expect(store.getState().language).toBe("en");
    });
  });
});
