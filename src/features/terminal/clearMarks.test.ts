/**
 * 화면 지우기 지점 저장: 세션별 마지막 지점만, 가장 최근 것부터 상한만큼 남기고,
 * 망가진 값·막힌 저장소에서도 앱을 멈추지 않는다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import { CLEAR_MARKS_RETAINED, loadClearMark, saveClearMark } from "./clearMarks";

const KEY = "iyagi.terminal-clear-marks.v1";

function stubStorage(initial: Record<string, string> = {}) {
  const data = new Map(Object.entries(initial));
  const storage = {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => void data.set(key, value),
  };
  vi.stubGlobal("localStorage", storage);
  return data;
}

afterEach(() => vi.unstubAllGlobals());

describe("clear marks", () => {
  it("remembers the last cleared seq per session", () => {
    stubStorage();
    expect(loadClearMark("s1")).toBeNull();
    saveClearMark("s1", 42);
    saveClearMark("s2", 7);
    expect(loadClearMark("s1")).toBe(42);
    expect(loadClearMark("s2")).toBe(7);
    saveClearMark("s1", 99);
    expect(loadClearMark("s1")).toBe(99);
  });

  it("keeps only the most recent sessions", () => {
    const data = stubStorage();
    for (let i = 1; i <= CLEAR_MARKS_RETAINED + 5; i++) saveClearMark(`s${i}`, i);
    expect(loadClearMark("s1")).toBeNull();
    expect(loadClearMark(`s${CLEAR_MARKS_RETAINED + 5}`)).toBe(CLEAR_MARKS_RETAINED + 5);
    expect(JSON.parse(data.get(KEY) ?? "[]")).toHaveLength(CLEAR_MARKS_RETAINED);
  });

  it("ignores invalid seqs and corrupt storage", () => {
    stubStorage({ [KEY]: "{not json" });
    expect(loadClearMark("s1")).toBeNull();
    saveClearMark("s1", 0);
    saveClearMark("s1", Number.NaN);
    expect(loadClearMark("s1")).toBeNull();
    stubStorage({ [KEY]: JSON.stringify([["s1", -3], ["s2", 5], "junk"]) });
    expect(loadClearMark("s1")).toBeNull();
    expect(loadClearMark("s2")).toBe(5);
  });

  it("keeps working when storage is missing or throws", () => {
    vi.stubGlobal("localStorage", undefined);
    expect(() => saveClearMark("s1", 3)).not.toThrow();
    expect(loadClearMark("s1")).toBeNull();
    vi.stubGlobal("localStorage", {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
    });
    expect(() => saveClearMark("s1", 3)).not.toThrow();
    expect(loadClearMark("s1")).toBeNull();
  });
});
