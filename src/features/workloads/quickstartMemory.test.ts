/** 퀵스타트 기억(W3-4): 저장·회수·손상값 방어. */
import { afterEach, describe, expect, it, vi } from "vitest";
import { getLastQuickstart, rememberQuickstart } from "./quickstartMemory";

describe("quickstartMemory", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("저장한 프로그램을 그대로 돌려준다", () => {
    const store = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => void store.set(key, value),
      removeItem: (key: string) => void store.delete(key),
    });
    expect(getLastQuickstart()).toBeNull();
    rememberQuickstart("/usr/local/bin/claude");
    expect(getLastQuickstart()).toBe("/usr/local/bin/claude");
  });

  it("빈 값·과장 값은 저장하지 않는다", () => {
    const store = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => void store.set(key, value),
      removeItem: (key: string) => void store.delete(key),
    });
    rememberQuickstart("");
    expect(getLastQuickstart()).toBeNull();
    rememberQuickstart("x".repeat(600));
    expect(getLastQuickstart()).toBeNull();
  });
});
