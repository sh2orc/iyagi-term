/**
 * 경량 i18n 코어 — 외부 의존성 없는 자체 사전.
 *
 * - 사전은 섹션 파일(sections/*.ts)의 ko/en 평면 키→문자열 레코드를
 *   index.ts가 registerMessages()로 병합한다. 키는 "area.name" 점 표기.
 * - 문자열 파라미터는 "{name}" 치환: t("app.tabTitle", { index: 2 }).
 * - 언어 선택: 저장된 값(setLanguage) > navigator 감지(ko* → ko, else en).
 *   node 테스트 환경(navigator 없음)에서는 항상 기본 언어(ko)라 SSR/시험이
 *   결정적이다.
 * - React 컴포넌트는 useI18n()이 언어 구독까지 담당해 전환 즉시 리렌더된다.
 *   비-React 코드(sessionController 등)는 t()를 호출 시점에 평가한다.
 * - ko에 없는 키/번역 누락 en 키는 코어가 기본 언어→키 문자열로 폴백하고,
 *   섹션별 ko/en 키 동일성은 i18n.test.ts가 강제한다.
 */

import { create } from "zustand";
import { persist } from "zustand/middleware";

export type Language = "ko" | "en";
export const LANGUAGES: readonly Language[] = ["ko", "en"];
export const DEFAULT_LANGUAGE: Language = "ko";

export type MessageParams = Record<string, string | number>;
export type Messages = Record<string, string>;
export interface SectionMessages {
  ko: Messages;
  en: Messages;
}

/**
 * navigator 기반 감지. window가 없으면(node 시험·SSR) 결정적 기본 언어 —
 * Node 21+는 navigator 전역이 있어 "navigator 부재" 검사로는 잡히지
 * 않아, 호스트 로케일이 시험 결과를 흔드는 결함(SOTA_GAP_REVIEW §8)이었다.
 */
export function detectLanguage(): Language {
  if (typeof window === "undefined") return DEFAULT_LANGUAGE;
  const tag = (navigator.language ?? "").toLowerCase();
  return tag.startsWith("ko") ? "ko" : "en";
}

/** {name} 치환 — 파라미터에 없는 자리표시자는 그대로 둔다. */
export function formatMessage(template: string, params?: MessageParams): string {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (whole, name: string) =>
    name in params ? String(params[name]) : whole,
  );
}

const bundles: Record<Language, Messages> = { ko: {}, en: {} };

/** 섹션 사전 병합(index.ts에서만 호출). */
export function registerMessages(section: SectionMessages): void {
  for (const lang of LANGUAGES) {
    Object.assign(bundles[lang], section[lang] ?? {});
  }
}

/** @internal 테스트/감사용 — 등록된 키의 언어별 사본. */
export function messagesSnapshot(lang: Language): Messages {
  return { ...bundles[lang] };
}

export function translate(lang: Language, key: string, params?: MessageParams): string {
  const raw = bundles[lang][key] ?? bundles[DEFAULT_LANGUAGE][key] ?? key;
  return formatMessage(raw, params);
}

// ------------------------------------------------------------------ store

export interface I18nState {
  /** null = 자동 감지(navigator). 사용자가 고르면 그 값이 저장된다. */
  language: Language | null;
  setLanguage: (language: Language | null) => void;
}

const STORAGE_KEY = "iyagi.language.v1";

export function createI18nStore(
  storage?: Pick<Storage, "getItem" | "setItem" | "removeItem">,
) {
  return create<I18nState>()(
    persist(
      (set) => ({
        language: null,
        setLanguage: (language) => set({ language }),
      }),
      {
        name: STORAGE_KEY,
        storage: {
          getItem: (name) => {
            const raw = storage?.getItem(name);
            return raw ? JSON.parse(raw) : null;
          },
          setItem: (name, value) => storage?.setItem(name, JSON.stringify(value)),
          removeItem: (name) => storage?.removeItem(name),
        },
        // persist merge는 봉투가 아닌 내부 state를 받는다(shellProfileStore
        // 패턴). 스키마가 다르면 조용히 자동 감게(null) 둔다.
        merge: (persisted, current) => {
          const state = persisted as { language?: unknown } | null;
          const lang = state?.language;
          return {
            ...current,
            language: lang === "ko" || lang === "en" ? lang : null,
          };
        },
      },
    ),
  );
}

export const useI18nStore = createI18nStore(
  typeof localStorage !== "undefined" ? localStorage : undefined,
);

export function resolveLanguage(stored: Language | null): Language {
  return stored ?? detectLanguage();
}

/** 호출 시점 언어로 번역(비-React 코드용). */
export function t(key: string, params?: MessageParams): string {
  return translate(resolveLanguage(useI18nStore.getState().language), key, params);
}

/** 언어 구독 + 그 언어로 묶인 t(React 컴포넌트용). */
export function useI18n(): { language: Language; t: (key: string, params?: MessageParams) => string } {
  const stored = useI18nStore((s) => s.language);
  const language = resolveLanguage(stored);
  return { language, t: (key, params) => translate(language, key, params) };
}
