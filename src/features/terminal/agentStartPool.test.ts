/**
 * 에이전트 시작 줄(P2-5) 계약: 동시 시작 수(평소 3, 압력 시 1), 시작 간격 1.5초,
 * 기동 확인까지 슬롯 유지(상한 12초), 메모리 CRITICAL 대기(상한 30초), 지금 시작,
 * 취소·무효 항목 정리, 대기 이유 표시.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { PaneStartWait } from "../../store/workbenchStore";
import {
  AgentStartPool,
  START_MEMORY_WAIT_CAP_MS,
  START_MIN_SPACING_MS,
  START_SETTLE_CAP_MS,
  startConcurrency,
  type AgentStartJob,
  type StartPressure,
} from "./agentStartPool";

interface FakeJob extends AgentStartJob {
  started: boolean;
  settle(): void;
  cancel(): void;
  droppedCount: number;
}

function job(id: string): FakeJob {
  let settled = false;
  let cancelled = false;
  const fake: FakeJob = {
    id,
    started: false,
    droppedCount: 0,
    start: async () => {
      fake.started = true;
    },
    settled: () => settled,
    cancelled: () => cancelled,
    dropped: () => {
      fake.droppedCount += 1;
    },
    settle: () => {
      settled = true;
    },
    cancel: () => {
      cancelled = true;
    },
  };
  return fake;
}

function harness(initial: StartPressure | null = null) {
  let pressure: StartPressure | null = initial;
  const waits = new Map<string, PaneStartWait | null>();
  const pool = new AgentStartPool({
    pressure: () => pressure,
    onWait: (id, wait) => waits.set(id, wait),
  });
  return {
    pool,
    waits,
    setPressure: (next: StartPressure | null) => {
      pressure = next;
    },
  };
}

const started = (jobs: FakeJob[]): string[] => jobs.filter((entry) => entry.started).map((entry) => entry.id);

describe("startConcurrency", () => {
  it("평소 3, 메모리 WARNING 이상이거나 CPU 압력 WARNING 이상이면 1", () => {
    expect(startConcurrency(null)).toBe(3);
    expect(startConcurrency({ memory: "NORMAL", cpu: "NORMAL" })).toBe(3);
    expect(startConcurrency({ memory: "WARNING", cpu: "NORMAL" })).toBe(1);
    expect(startConcurrency({ memory: "NORMAL", cpu: "WARNING" })).toBe(1);
    expect(startConcurrency({ memory: "NORMAL", cpu: "CRITICAL" })).toBe(1);
  });
});

describe("AgentStartPool", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("첫 항목은 곧바로 시작하고(약속을 돌려준다) 다음은 1.5초 간격으로, 동시 3개까지만", async () => {
    const h = harness();
    const jobs = ["a", "b", "c", "d"].map(job);
    expect(h.pool.enqueue(jobs[0])).toBeInstanceOf(Promise);
    expect(h.pool.enqueue(jobs[1])).toBeNull();
    h.pool.enqueue(jobs[2]);
    h.pool.enqueue(jobs[3]);
    expect(started(jobs)).toEqual(["a"]);
    expect(h.waits.get("b")).toBe("turn");

    await vi.advanceTimersByTimeAsync(START_MIN_SPACING_MS);
    expect(started(jobs)).toEqual(["a", "b"]);
    expect(h.waits.get("b")).toBeNull();
    await vi.advanceTimersByTimeAsync(START_MIN_SPACING_MS);
    expect(started(jobs)).toEqual(["a", "b", "c"]);
    // 슬롯 셋을 모두 쥐었다 — 간격이 지나도 넷째는 기다린다.
    await vi.advanceTimersByTimeAsync(START_MIN_SPACING_MS * 2);
    expect(started(jobs)).toEqual(["a", "b", "c"]);
    expect(h.pool.activeCount).toBe(3);

    // 기동 확인(settled)이 오면 그 슬롯을 놓고 다음을 띄운다.
    jobs[0].settle();
    await vi.advanceTimersByTimeAsync(300);
    expect(started(jobs)).toEqual(["a", "b", "c", "d"]);
    h.pool.dispose();
  });

  it("기동 확인이 없어도 시작 뒤 12초면 슬롯을 놓는다", async () => {
    const h = harness({ memory: "WARNING", cpu: "NORMAL" });
    const jobs = ["a", "b"].map(job);
    h.pool.enqueue(jobs[0]);
    h.pool.enqueue(jobs[1]);
    // 압력 중에는 하나씩: 간격이 지나도 둘째는 첫째의 슬롯을 기다린다.
    await vi.advanceTimersByTimeAsync(START_SETTLE_CAP_MS - 500);
    expect(started(jobs)).toEqual(["a"]);
    await vi.advanceTimersByTimeAsync(800);
    expect(started(jobs)).toEqual(["a", "b"]);
    h.pool.dispose();
  });

  it("메모리 CRITICAL이면 머리 항목이 '메모리'로 기다리고, 풀리면 곧 시작하며, 30초가 지나면 그냥 시작한다", async () => {
    const h = harness({ memory: "CRITICAL", cpu: "NORMAL" });
    const jobs = ["a", "b"].map(job);
    h.pool.enqueue(jobs[0]);
    h.pool.enqueue(jobs[1]);
    expect(started(jobs)).toEqual([]);
    expect(h.waits.get("a")).toBe("memory");
    expect(h.waits.get("b")).toBe("memory");

    h.setPressure({ memory: "NORMAL", cpu: "NORMAL" });
    await vi.advanceTimersByTimeAsync(300);
    expect(started(jobs)).toEqual(["a"]);
    expect(h.waits.get("b")).toBe("turn");

    // 둘째가 머리가 된 뒤 다시 CRITICAL — 상한까지 기다리고 그 뒤에는 시작한다.
    h.setPressure({ memory: "CRITICAL", cpu: "NORMAL" });
    jobs[0].settle();
    await vi.advanceTimersByTimeAsync(START_MEMORY_WAIT_CAP_MS - 1000);
    expect(started(jobs)).toEqual(["a"]);
    await vi.advanceTimersByTimeAsync(1500);
    expect(started(jobs)).toEqual(["a", "b"]);
    h.pool.dispose();
  });

  it("지금 시작은 줄·압력·간격을 건너뛴다", async () => {
    const h = harness({ memory: "CRITICAL", cpu: "CRITICAL" });
    const jobs = ["a", "b"].map(job);
    h.pool.enqueue(jobs[0]);
    h.pool.enqueue(jobs[1]);
    expect(started(jobs)).toEqual([]);
    expect(h.pool.startNow("b")).toBe(true);
    expect(started(jobs)).toEqual(["b"]);
    expect(h.waits.get("b")).toBeNull();
    expect(h.pool.isQueued("a")).toBe(true);
    expect(h.pool.startNow("nope")).toBe(false);
    h.pool.dispose();
  });

  it("무효가 된 항목은 시작하지 않고 버리며(대기 표시를 지운다) 취소도 같다", async () => {
    const h = harness();
    const jobs = ["a", "b", "c"].map(job);
    h.pool.enqueue(jobs[0]);
    h.pool.enqueue(jobs[1]);
    h.pool.enqueue(jobs[2]);
    jobs[1].cancel();
    h.pool.cancel("c");
    expect(h.waits.get("c")).toBeNull();
    expect(jobs[2].droppedCount).toBe(1);
    await vi.advanceTimersByTimeAsync(START_MIN_SPACING_MS * 3);
    expect(started(jobs)).toEqual(["a"]);
    expect(jobs[1].droppedCount).toBe(1);
    expect(h.waits.get("b")).toBeNull();
    expect(h.pool.queuedCount).toBe(0);
    h.pool.dispose();
  });

  it("시작이 실패해도 슬롯은 정산되고, 할 일이 없으면 타이머를 남기지 않는다", async () => {
    const h = harness();
    const failing = job("a");
    failing.start = async () => {
      throw new Error("launch exploded");
    };
    await expect(h.pool.enqueue(failing)).rejects.toThrow("launch exploded");
    failing.settle();
    await vi.advanceTimersByTimeAsync(300);
    expect(h.pool.activeCount).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
    h.pool.dispose();
  });

  it("폐기하면 기다리던 항목을 버리고 더 시작하지 않는다", async () => {
    const h = harness();
    const jobs = ["a", "b"].map(job);
    h.pool.enqueue(jobs[0]);
    h.pool.enqueue(jobs[1]);
    h.pool.dispose();
    await vi.advanceTimersByTimeAsync(START_MIN_SPACING_MS * 2);
    expect(started(jobs)).toEqual(["a"]);
    expect(jobs[1].droppedCount).toBe(1);
    expect(h.pool.enqueue(job("c"))).toBeNull();
    expect(vi.getTimerCount()).toBe(0);
  });
});
