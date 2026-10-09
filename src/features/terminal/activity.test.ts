import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  ACTIVITY_IDLE_MS,
  clearTerminalActivity,
  markTerminalActivity,
  recordSessionOutput,
  sessionLastOutputAt,
  useTerminalActivity,
} from "./activity";

describe("terminal activity", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => { for (const id of useTerminalActivity.getState().sessions) clearTerminalActivity(id); vi.useRealTimers(); });
  it("goes idle after three seconds and renews only for new activity", () => {
    expect(useTerminalActivity.getState().sessions.size).toBe(0);
    markTerminalActivity("a");
    vi.advanceTimersByTime(2000);
    const snapshot = useTerminalActivity.getState();
    markTerminalActivity("a");
    expect(useTerminalActivity.getState()).toBe(snapshot);
    vi.advanceTimersByTime(2000);
    expect(useTerminalActivity.getState().sessions.has("a")).toBe(true);
    vi.advanceTimersByTime(1000);
    expect(useTerminalActivity.getState().sessions.has("a")).toBe(false);
  });
  it("clears exited sessions immediately without affecting other tabs", () => {
    markTerminalActivity("a"); markTerminalActivity("b");
    clearTerminalActivity("a");
    expect([...useTerminalActivity.getState().sessions]).toEqual(["b"]);
    vi.advanceTimersByTime(ACTIVITY_IDLE_MS);
    expect(useTerminalActivity.getState().sessions.size).toBe(0);
  });
});

describe("마지막 출력 시각(경과 배지용)", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => { for (const id of useTerminalActivity.getState().sessions) clearTerminalActivity(id); vi.useRealTimers(); });

  it("idle 전환으로 지워지지 않고 세션 종료 때 지워진다", () => {
    recordSessionOutput("a");
    const at = sessionLastOutputAt("a");
    expect(at).not.toBeNull();
    vi.advanceTimersByTime(ACTIVITY_IDLE_MS * 10);
    // 3초 idle 창은 활동 점만 끈다 — 경과 배지의 시각은 남는다.
    expect(sessionLastOutputAt("a")).toBe(at);
    clearTerminalActivity("a");
    expect(sessionLastOutputAt("a")).toBeNull();
  });

  it("활동 점이 3초 idle로 꺼져도 마지막 출력 시각은 남는다", () => {
    markTerminalActivity("a");
    recordSessionOutput("a");
    const at = sessionLastOutputAt("a");
    vi.advanceTimersByTime(ACTIVITY_IDLE_MS + 1);
    // 활동 점(3초 창)만 꺼진다 — 경과 배지의 시각은 세션이 끝날 때까지 남아야 한다.
    expect(useTerminalActivity.getState().sessions.has("a")).toBe(false);
    expect(sessionLastOutputAt("a")).toBe(at);
    clearTerminalActivity("a");
    expect(sessionLastOutputAt("a")).toBeNull();
  });

  it("출력이 다시 오면 시각을 갱신한다", () => {
    recordSessionOutput("a");
    const first = sessionLastOutputAt("a");
    vi.advanceTimersByTime(1000);
    recordSessionOutput("a");
    expect(sessionLastOutputAt("a")!).toBeGreaterThan(first!);
  });

  it("기록 없는 세션은 null — 배지를 띄우지 않는 근거", () => {
    expect(sessionLastOutputAt("unknown")).toBeNull();
  });
});
