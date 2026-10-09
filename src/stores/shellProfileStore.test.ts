import { describe, expect, it, vi } from "vitest";
import { createShellProfileStore, type ShellProfileState } from "./shellProfileStore";
import { customProfile } from "../features/terminal/shellProfiles";

/** localStorage 대체 — 지속화 라운드트립 검증용(최소 인터페이스). */
class MemoryStorage implements Pick<Storage, "getItem" | "setItem" | "removeItem"> {
  private map = new Map<string, string>();
  getItem(name: string): string | null {
    return this.map.get(name) ?? null;
  }
  setItem(name: string, value: string): void {
    this.map.set(name, value);
  }
  removeItem(name: string): void {
    this.map.delete(name);
  }
}

/** 상태 스냅샷은 불변이므로 매 호출 후 fresh하게 읽는다. */
function fresh(): { get: () => ShellProfileState; storage: MemoryStorage } {
  const storage = new MemoryStorage();
  const useStore = createShellProfileStore(storage);
  return { get: () => useStore.getState(), storage };
}

describe("shellProfileStore", () => {
  it("기본 프로필 설정/해제", () => {
    const { get } = fresh();
    expect(get().defaultProfileId).toBeNull();
    get().setDefault("powershell");
    expect(get().defaultProfileId).toBe("powershell");
    get().setDefault(null);
    expect(get().defaultProfileId).toBeNull();
  });

  it("커스텀 프로필 추가/삭제, 중복 id 거부", () => {
    const { get } = fresh();
    const profile = customProfile("내 셸", "C:\\tools\\nu.exe", ["-l"]);
    expect(get().addCustom(profile)).toBe(true);
    expect(get().addCustom(profile)).toBe(false);
    expect(get().custom).toHaveLength(1);
    get().setDefault(profile.id);
    get().removeCustom(profile.id);
    expect(get().custom).toHaveLength(0);
    // 삭제된 기본 id는 null로 정리된다.
    expect(get().defaultProfileId).toBeNull();
  });

  it("localStorage 지속화 라운드트립", async () => {
    const { storage } = fresh();
    const first = createShellProfileStore(storage);
    first.getState().setDefault("wsl:Ubuntu");
    first.getState().addCustom(customProfile("bash", "/usr/bin/bash", []));
    // persist의 쓰기/수화 모두 비동기 — 폴링으로 완료를 기다린다.
    const second = createShellProfileStore(storage);
    await vi.waitFor(() => {
      expect(second.getState().defaultProfileId).toBe("wsl:Ubuntu");
    });
    const state = second.getState();
    expect(state.custom.map((p) => p.id)).toEqual(["custom:bash"]);
  });

  it("스키마가 다른 저장값은 거부(빈 상태로 시작)", async () => {
    const storage = new MemoryStorage();
    storage.setItem(
      "iyagi.shellprofiles.v1",
      JSON.stringify({ state: { defaultProfileId: 123, custom: [{ bad: true }] } }),
    );
    const store = createShellProfileStore(storage);
    await vi.waitFor(() => {
      expect(store.persist.hasHydrated()).toBe(true);
    });
    const state = store.getState();
    expect(state.defaultProfileId).toBeNull();
    expect(state.custom).toHaveLength(0);
  });
});
