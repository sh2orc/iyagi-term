/**
 * 셸 프로필 저장소: 기본 프로필 선택 + 사용자 추가 프로필.
 * localStorage에 버전 묶음으로 지속한다(프로필 스토어와 동일 패턴).
 */

import { create } from "zustand";
import { persist } from "zustand/middleware";
import type { ShellProfile } from "../features/terminal/shellProfiles";

const STORAGE_KEY = "iyagi.shellprofiles.v1";

export interface ShellProfileState {
  /** 사용자가 고른 기본 프로필 id(null이면 플랫폼 관례). */
  defaultProfileId: string | null;
  /** 사용자가 추가한 프로필(내장 프로필은 프로브가 제공). */
  custom: ShellProfile[];
  setDefault: (id: string | null) => void;
  addCustom: (profile: ShellProfile) => boolean;
  removeCustom: (id: string) => void;
}

function isProfile(value: unknown): value is ShellProfile {
  if (typeof value !== "object" || value === null) return false;
  const p = value as Record<string, unknown>;
  return (
    typeof p.id === "string" &&
    typeof p.label === "string" &&
    typeof p.program === "string" &&
    Array.isArray(p.argv) &&
    typeof p.kind === "string" &&
    typeof p.builtin === "boolean"
  );
}

export function createShellProfileStore(storage?: Pick<Storage, "getItem" | "setItem" | "removeItem">) {
  return create<ShellProfileState>()(
    persist(
      (set, get) => ({
        defaultProfileId: null,
        custom: [],
        setDefault: (id) => set({ defaultProfileId: id }),
        addCustom: (profile) => {
          if (get().custom.some((p) => p.id === profile.id)) return false;
          if (!isProfile(profile)) return false;
          set({ custom: [...get().custom, profile] });
          return true;
        },
        removeCustom: (id) =>
          set({
            custom: get().custom.filter((p) => p.id !== id),
            defaultProfileId: get().defaultProfileId === id ? null : get().defaultProfileId,
          }),
      }),
      {
        name: STORAGE_KEY,
        // PersistStorage 계약: persist는 StorageValue 객체를 주고받는다 —
        // 어댑터가 실제 저장소에서 JSON 직렬화를 담당한다.
        storage: {
          getItem: (name) => {
            const raw = storage?.getItem(name);
            if (!raw) return null;
            try {
              return JSON.parse(raw);
            } catch {
              return null;
            }
          },
          setItem: (name, value) => storage?.setItem(name, JSON.stringify(value)),
          removeItem: (name) => storage?.removeItem(name),
        },
        // 버전/구조가 다른 저장값은 조용히 버린다(스키마 불일치 거부).
        // persist는 merge에 봉투가 아닌 내부 state를 전달한다.
        merge: (persisted, current) => {
          const state = persisted as Record<string, unknown> | null;
          if (
            !state ||
            !Array.isArray(state.custom) ||
            !state.custom.every(isProfile)
          ) {
            return current;
          }
          return {
            ...current,
            defaultProfileId:
              typeof state.defaultProfileId === "string" ? state.defaultProfileId : null,
            custom: state.custom,
          };
        },
      },
    ),
  );
}

export const useShellProfileStore = createShellProfileStore(
  typeof localStorage !== "undefined" ? localStorage : undefined,
);
