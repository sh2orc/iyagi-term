/**
 * 작업별 마지막 모습(제목·에이전트) 저장: 제목 문자열과 아는 에이전트 id만,
 * 최근 것부터 상한만큼 남기고, 망가진 값·막힌 저장소에서도 앱을 멈추지 않는다.
 */

import { afterEach, describe, expect, it, vi } from "vitest";
import {
  WORKLOAD_MEMORY_RETAINED,
  WORKLOAD_MEMORY_SAVE_INTERVAL_MS,
  WORKLOAD_MEMORY_STORAGE_KEY,
  decodeWorkloadMemory,
  encodeWorkloadMemory,
  flushWorkloadMemorySave,
  isShellExitTitle,
  readWorkloadMemory,
  rememberWorkloadAgents,
  rememberWorkloadRecovery,
  rememberWorkloadTitle,
  rememberedWorkload,
  saveWorkloadMemory,
  scheduleWorkloadMemorySave,
  type RememberedWorkload,
} from "./workloadMemory";

function stubStorage(initial: Record<string, string> = {}): Map<string, string> {
  const data = new Map(Object.entries(initial));
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => void data.set(key, value),
  });
  return data;
}

afterEach(() => {
  flushWorkloadMemorySave();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("workload memory codec", () => {
  it("round-trips titles and agents in insertion order", () => {
    const memory: Record<string, RememberedWorkload> = {
      "w-1": { title: "✳ Fix login bug", agent: "claude" },
      "w-2": { title: "vim README.md", agent: null },
      "w-3": { title: null, agent: "opencode" },
    };
    const encoded = encodeWorkloadMemory(memory);
    expect(JSON.parse(encoded)).toEqual([
      ["w-1", "✳ Fix login bug", "claude"],
      ["w-2", "vim README.md", null],
      ["w-3", null, "opencode"],
    ]);
    const decoded = decodeWorkloadMemory(encoded);
    expect(decoded).toEqual(memory);
    expect(Object.keys(decoded)).toEqual(["w-1", "w-2", "w-3"]);
  });

  it("keeps only the most recent entries", () => {
    const memory = Object.fromEntries(
      Array.from({ length: WORKLOAD_MEMORY_RETAINED + 3 }, (_, index) => [`w-${index}`, { title: `t${index}`, agent: null }]),
    );
    const decoded = decodeWorkloadMemory(encodeWorkloadMemory(memory));
    expect(Object.keys(decoded)).toHaveLength(WORKLOAD_MEMORY_RETAINED);
    expect(decoded["w-0"]).toBeUndefined();
    expect(decoded[`w-${WORKLOAD_MEMORY_RETAINED + 2}`]).toEqual({ title: `t${WORKLOAD_MEMORY_RETAINED + 2}`, agent: null });
  });

  it("drops corrupt entries one by one and re-sanitizes stored values", () => {
    const raw = JSON.stringify([
      "junk",
      ["", "no id", null],
      ["x".repeat(129), "id too long", null],
      ["w-ctrl", "]0;evil title ", "claude"],
      ["w-long", "y".repeat(500), null],
      ["w-unknown-agent", null, "aider"],
      ["w-empty", "   ", null],
      ["w-types", 42, 7],
      ["__proto__", "own property", "codex"],
    ]);
    const decoded = decodeWorkloadMemory(raw);
    expect(Object.keys(decoded).sort()).toEqual(["__proto__", "w-ctrl", "w-long"].sort());
    expect(decoded["w-ctrl"]).toEqual({ title: "]0;evil title", agent: "claude" });
    expect(decoded["w-long"]?.title).toHaveLength(200);
    // `__proto__`도 평범한 자기 속성이다 — 프로토타입을 바꾸지 않는다.
    expect(Object.getPrototypeOf(decoded)).toBe(Object.prototype);
    expect(rememberedWorkload(decoded, "__proto__")).toEqual({ title: "own property", agent: "codex" });
    expect(decodeWorkloadMemory("{not json")).toEqual({});
    expect(decodeWorkloadMemory(JSON.stringify({ "w-1": "title" }))).toEqual({});
    expect(decodeWorkloadMemory(null)).toEqual({});
  });

  it("stores the workload that recovered an entry and drops a self-reference", () => {
    const memory = {
      "w-1": { title: "sh2orc@mac:~/project", agent: "claude", recoveredBy: "w-2" },
      "w-2": { title: "✳ Fix login bug", agent: "claude" },
    };
    const encoded = encodeWorkloadMemory(memory);
    expect(JSON.parse(encoded)).toEqual([
      ["w-1", "sh2orc@mac:~/project", "claude", "w-2"],
      ["w-2", "✳ Fix login bug", "claude"],
    ]);
    expect(decodeWorkloadMemory(encoded)).toEqual(memory);
    // 아무것도 기억하지 않았어도 이어받은 작업만으로 항목이 남는다. 자기 자신·잘못된 값은 버린다.
    expect(decodeWorkloadMemory(JSON.stringify([["w-3", null, null, "w-4"], ["w-5", null, null, "w-5"], ["w-6", null, null, 42]]))).toEqual({
      "w-3": { title: null, agent: null, recoveredBy: "w-4" },
    });
  });

  it("keeps every remembered field when another one changes", () => {
    let memory = rememberWorkloadRecovery({}, "w-1", "w-2");
    expect(memory).toEqual({ "w-1": { title: null, agent: null, recoveredBy: "w-2" } });
    memory = rememberWorkloadTitle(memory, "w-1", "sh2orc@mac:~/project");
    memory = rememberWorkloadAgents(memory, [{ workload_id: "w-1", agent: { agent: "codex", pid: 1, detected_at_ms: 1 } } as never]);
    expect(memory["w-1"]).toEqual({ title: "sh2orc@mac:~/project", agent: "codex", recoveredBy: "w-2" });
    expect(rememberWorkloadRecovery(memory, "w-1", "w-2")).toBe(memory);
    expect(rememberWorkloadRecovery(memory, "w-9", "w-9")).toBe(memory);
  });

  it("reads only own entries", () => {
    expect(rememberedWorkload({}, "toString")).toBeNull();
    const memory = rememberWorkloadTitle({}, "constructor", "own");
    expect(rememberedWorkload(memory, "constructor")).toEqual({ title: "own", agent: null });
    expect(rememberWorkloadTitle(memory, "constructor", "own")).toBe(memory);
  });
});

describe("shell exit titles", () => {
  it("recognizes only the commands that end the shell", () => {
    for (const title of ["exit", "exit 1", "exit -1", " logout ", "bye"]) expect(isShellExitTitle(title)).toBe(true);
    for (const title of ["exit now", "exiting", "git commit -m exit", "Exit", "vim exit.txt", ""]) {
      expect(isShellExitTitle(title)).toBe(false);
    }
  });

  it("never replaces the remembered title", () => {
    const memory = rememberWorkloadTitle({}, "w-1", "sh2orc@mac:~/project");
    expect(rememberWorkloadTitle(memory, "w-1", "exit")).toBe(memory);
    expect(rememberWorkloadTitle({}, "w-2", "logout")).toEqual({});
  });
});

describe("workload memory storage", () => {
  it("saves when the memory object changes and reads it back", () => {
    const data = stubStorage();
    const memory = { "w-1": { title: "✳ Fix login bug", agent: "claude" } };
    saveWorkloadMemory(memory);
    expect(JSON.parse(data.get(WORKLOAD_MEMORY_STORAGE_KEY) ?? "null")).toEqual([["w-1", "✳ Fix login bug", "claude"]]);
    expect(readWorkloadMemory()).toEqual(memory);
    // 같은 객체면 다시 쓰지 않는다(store의 다른 변경마다 불린다).
    data.delete(WORKLOAD_MEMORY_STORAGE_KEY);
    saveWorkloadMemory(memory);
    expect(data.has(WORKLOAD_MEMORY_STORAGE_KEY)).toBe(false);
    saveWorkloadMemory({ ...memory, "w-2": { title: null, agent: "codex" } });
    expect(readWorkloadMemory()).toEqual({ ...memory, "w-2": { title: null, agent: "codex" } });
  });

  it("writes the first change at once and coalesces a spinner burst into one write per interval", () => {
    vi.useFakeTimers();
    vi.setSystemTime(10_000_000);
    const writes: string[] = [];
    vi.stubGlobal("localStorage", {
      getItem: () => writes.at(-1) ?? null,
      setItem: (_key: string, value: string) => void writes.push(value),
    });
    let memory = rememberWorkloadTitle({}, "w-1", "sh2orc@mac:~/project");
    scheduleWorkloadMemorySave(memory);
    expect(writes).toHaveLength(1);
    // 스피너 프레임(100ms 간격) 20개 — 간격 안에서는 쓰지 않는다.
    const frames = ["⠋", "⠙", "⠹", "⠸"];
    for (let frame = 0; frame < 20; frame++) {
      vi.advanceTimersByTime(100);
      memory = rememberWorkloadTitle(memory, "w-1", `${frames[frame % frames.length]} Fix login bug`);
      scheduleWorkloadMemorySave(memory);
      // store의 다른 변경도 같은 객체로 다시 부른다.
      scheduleWorkloadMemorySave(memory);
    }
    expect(writes.length).toBeLessThanOrEqual(3);
    vi.advanceTimersByTime(WORKLOAD_MEMORY_SAVE_INTERVAL_MS);
    // 저장되는 제목에는 스피너 글리프가 없다(프레임이 달라도 같은 값 → 중복 쓰기 없음).
    expect(readWorkloadMemory()).toEqual({ "w-1": { title: "Fix login bug", agent: null } });
    const settled = writes.length;
    vi.advanceTimersByTime(WORKLOAD_MEMORY_SAVE_INTERVAL_MS * 5);
    expect(writes).toHaveLength(settled);
  });

  it("flushes the pending change on demand", () => {
    vi.useFakeTimers();
    vi.setSystemTime(20_000_000);
    const data = stubStorage();
    scheduleWorkloadMemorySave({ "w-1": { title: "first", agent: null } });
    scheduleWorkloadMemorySave({ "w-1": { title: "last", agent: "codex" } });
    expect(readWorkloadMemory()).toEqual({ "w-1": { title: "first", agent: null } });
    flushWorkloadMemorySave();
    expect(JSON.parse(data.get(WORKLOAD_MEMORY_STORAGE_KEY) ?? "null")).toEqual([["w-1", "last", "codex"]]);
  });

  it("keeps working when storage is missing or throws", () => {
    vi.stubGlobal("localStorage", undefined);
    expect(readWorkloadMemory()).toEqual({});
    expect(() => saveWorkloadMemory({ "w-1": { title: "a", agent: null } })).not.toThrow();
    vi.stubGlobal("localStorage", {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("quota");
      },
    });
    expect(readWorkloadMemory()).toEqual({});
    expect(() => saveWorkloadMemory({ "w-1": { title: "b", agent: null } })).not.toThrow();
  });
});
